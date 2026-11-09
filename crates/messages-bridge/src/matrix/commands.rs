use matrix_sdk::{
    room::MessagesOptions,
    ruma::{EventId, OwnedTransactionId, RoomId},
};
use messages_core::RoomKey;
use serde_json::{Value, json};

use super::Matrix;
use crate::Command;

impl Matrix {
    pub(super) async fn dispatch(
        &self,
        operation: &str,
        command: Command,
    ) -> anyhow::Result<Value> {
        if operation == "attachment" {
            return self.download_attachment(&command).await;
        }
        let _write = self.writes.lock().await;
        if operation == "open" {
            use matrix_sdk::ruma::{RoomOrAliasId, UserId};
            let recipient = command.chat_id.trim();
            let mut created_encrypted = false;
            let room = if recipient.starts_with('@') {
                let user = UserId::parse(recipient)?;
                if let Some(room) = self.client.get_dm_room(&user) {
                    room
                } else {
                    created_encrypted = true;
                    self.client.create_dm(&user).await?
                }
            } else {
                self.client
                    .join_room_by_id_or_alias(&RoomOrAliasId::parse(recipient)?, &[])
                    .await?
            };
            let key = RoomKey::new(uuid::Uuid::nil(), room.room_id().as_str());
            let encrypted = room.latest_encryption_state().await?.is_encrypted();
            anyhow::ensure!(
                !created_encrypted || encrypted,
                "The server did not enable encryption for this direct conversation"
            );
            let mut cache = self.cache.lock().await;
            if cache.store.room(&key).is_none() {
                cache
                    .store
                    .conversations
                    .push(messages_core::Conversation::empty(
                        key.clone(),
                        recipient,
                        if recipient.starts_with('@') {
                            messages_core::RoomKind::Direct
                        } else {
                            messages_core::RoomKind::Group
                        },
                        if encrypted {
                            messages_core::Encryption::EndToEnd
                        } else {
                            messages_core::Encryption::Transport
                        },
                        super::capabilities(),
                    ));
                cache.persist().await?;
            }
            return Ok(json!({"conversation":cache.store.room(&key)}));
        }
        let id = RoomId::parse(&command.chat_id)?;
        let room = self
            .client
            .get_room(&id)
            .ok_or_else(|| anyhow::anyhow!("Wait for room synchronization"))?;
        let key = RoomKey::new(uuid::Uuid::nil(), &command.chat_id);
        if operation == "history" {
            let page = room
                .messages(MessagesOptions::backward().from(command.before.as_deref()))
                .await?;
            let own = self.client.user_id().map(|id| id.as_str()).unwrap_or("");
            let mut cache = self.cache.lock().await;
            for event in page.chunk.into_iter().rev() {
                cache.apply_matrix(&key, &serde_json::from_str(event.raw().json().get())?, own)?;
            }
            if let Some(room) = cache.store.room_mut(&key) {
                room.has_older = page.end.is_some();
                room.pagination = page.end.clone();
            }
            cache.persist().await?;
            self.decryption_needed.notify_one();
            let messages = cache
                .store
                .room(&key)
                .map(|room| room.messages.clone())
                .unwrap_or_default();
            return Ok(json!({"messages":messages,"next":page.end}));
        }
        if operation == "typing" {
            room.typing_notice(command.active).await?;
            return Ok(json!({"ok":true}));
        }
        let expected_encrypted = self
            .cache
            .lock()
            .await
            .store
            .room(&key)
            .is_some_and(|room| room.encryption == messages_core::Encryption::EndToEnd);
        anyhow::ensure!(
            !expected_encrypted || room.latest_encryption_state().await?.is_encrypted(),
            "Encryption is unavailable for this encrypted conversation"
        );
        if operation == "send" {
            if !command.attachments.is_empty() {
                return self.send_files(&room, command).await;
            }
            anyhow::ensure!(!command.body.trim().is_empty(), "Empty message");
            let transaction = command
                .transaction_id
                .ok_or_else(|| anyhow::anyhow!("Missing transaction ID"))?;
            let mut content = json!({"msgtype":"m.text","body":command.body});
            if let Some(reply) = command.reply_to {
                content["m.relates_to"] = json!({"m.in_reply_to":{"event_id":reply}});
            }
            let transaction = OwnedTransactionId::from(transaction.to_string());
            let result = room
                .send_raw("m.room.message", content.clone())
                .with_transaction_id(&transaction)
                .await?;
            self.record_event(
                &key,
                result.response.event_id.as_str(),
                "m.room.message",
                content,
            )
            .await?;
            return Ok(json!({"message_id":result.response.event_id}));
        }
        let id = EventId::parse(&command.message_id)?;
        if operation == "read" {
            room.send_single_receipt(
                matrix_sdk::ruma::api::client::receipt::create_receipt::v3::ReceiptType::Read,
                matrix_sdk::ruma::events::receipt::ReceiptThread::Unthreaded,
                id,
            )
            .await?;
            return Ok(json!({"ok":true}));
        }
        let target = self
            .cache
            .lock()
            .await
            .store
            .room(&key)
            .and_then(|room| {
                room.messages
                    .iter()
                    .find(|message| message.id == command.message_id)
            })
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Message is unavailable"))?;
        match operation {
            "react" => {
                let own = self.client.user_id().map(|id| id.as_str()).unwrap_or("");
                if command.remove {
                    let ids = self.cache.lock().await.matrix.own_reaction_ids(
                        &key.room,
                        id.as_str(),
                        &command.emoji,
                        own,
                    );
                    for reaction_id in ids {
                        let result = room
                            .redact(&EventId::parse(&reaction_id)?, None, None)
                            .await?;
                        self.record_event(
                            &key,
                            result.event_id.as_str(),
                            "m.room.redaction",
                            json!({"redacts":reaction_id}),
                        )
                        .await?;
                    }
                } else if self
                    .cache
                    .lock()
                    .await
                    .matrix
                    .own_reaction_ids(&key.room, id.as_str(), &command.emoji, own)
                    .is_empty()
                {
                    let content = json!({"m.relates_to":{"rel_type":"m.annotation","event_id":id,"key":command.emoji}});
                    let result = room.send_raw("m.reaction", content.clone()).await?;
                    self.record_event(
                        &key,
                        result.response.event_id.as_str(),
                        "m.reaction",
                        content,
                    )
                    .await?;
                }
            }
            "edit" => {
                anyhow::ensure!(target.own, "Cannot edit another sender's message");
                let content = json!({"msgtype":"m.text","body":format!("* {}",command.body),
                    "m.new_content":{"msgtype":"m.text","body":command.body},
                    "m.relates_to":{"rel_type":"m.replace","event_id":id}});
                let result = room.send_raw("m.room.message", content.clone()).await?;
                self.record_event(
                    &key,
                    result.response.event_id.as_str(),
                    "m.room.message",
                    content,
                )
                .await?;
            }
            "delete" => {
                anyhow::ensure!(target.own, "Cannot delete another sender's message");
                let result = room.redact(&id, None, None).await?;
                self.record_event(
                    &key,
                    result.event_id.as_str(),
                    "m.room.redaction",
                    json!({"redacts":id}),
                )
                .await?;
            }
            _ => anyhow::bail!("Unsupported operation"),
        }
        Ok(json!({"ok":true}))
    }

    async fn record_event(
        &self,
        key: &RoomKey,
        id: &str,
        kind: &str,
        content: Value,
    ) -> anyhow::Result<()> {
        let own = self.client.user_id().map(|id| id.as_str()).unwrap_or("");
        let event = json!({"event_id":id,"type":kind,"sender":own,"origin_server_ts":chrono::Utc::now().timestamp_millis(),"content":content});
        let mut cache = self.cache.lock().await;
        cache.apply_matrix(key, &event, own)?;
        cache.persist().await
    }
}
