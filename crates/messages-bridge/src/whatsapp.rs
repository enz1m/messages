use std::{path::Path, sync::Arc, time::Instant};

use async_trait::async_trait;
use chrono::Utc;
use messages_core::{
    Capabilities, Capability, ChatMessage, Delivery, Encryption, RoomKey, RoomKind,
};
use messages_protocols::bridge::{API_VERSION, BridgeStatus};
use serde_json::{Value, json};
use tokio::sync::{Mutex, RwLock};
use whatsapp_rust::prelude::*;

use crate::{
    Backend, Command,
    cache::{Cache, SharedCache},
};

mod call_media;
mod calls;
pub(crate) mod events;
mod history;
mod media;
mod profiles;
#[cfg(test)]
mod tests;

#[derive(Default)]
struct Session {
    connected: bool,
    qr: Option<(String, Instant)>,
    error: bool,
}

pub struct WhatsApp {
    client: Arc<Client>,
    cache: SharedCache,
    session: Arc<RwLock<Session>>,
    writes: Mutex<()>,
    calls: Arc<calls::Manager>,
    contacts_requested: std::sync::Mutex<Option<Instant>>,
    identity_cursor: std::sync::atomic::AtomicUsize,
}

fn capabilities() -> Capabilities {
    Capabilities::new([
        Capability::StartConversation,
        Capability::SendText,
        Capability::AttachFiles,
        Capability::Typing,
        Capability::ReadReceipts,
        Capability::Reply,
        Capability::React,
        Capability::EditOwn,
        Capability::DeleteOwn,
        Capability::VoiceCall,
        Capability::VideoCall,
    ])
}

impl WhatsApp {
    pub async fn prepare(directory: &Path) -> anyhow::Result<(Arc<Self>, Bot)> {
        let calls = Arc::new(calls::Manager::default());
        let event_calls = calls.clone();
        let cache = Cache::open(directory.join("messages.json")).await?;
        {
            let mut locked = cache.lock().await;
            profiles::repair(&mut locked);
        }
        let session = Arc::new(RwLock::new(Session::default()));
        let qr_session = session.clone();
        let connected_session = session.clone();
        let event_session = session.clone();
        let event_cache = cache.clone();
        let bot = Bot::builder()
            .with_backend(
                SqliteStore::open(directory.join("session.db").to_string_lossy().as_ref()).await?,
            )
            .with_event_delivery(EventDelivery::Ordered { capacity: 256 })
            .on_qr_code(move |code, timeout| {
                let session = qr_session.clone();
                async move {
                    session.write().await.qr = Some((code, Instant::now() + timeout));
                }
            })
            .on_connected(move |_| {
                let session = connected_session.clone();
                async move {
                    let mut session = session.write().await;
                    session.connected = true;
                    session.qr = None;
                    session.error = false;
                }
            })
            .on_event(move |event, client| {
                let session = event_session.clone();
                let calls = event_calls.clone();
                let cache = event_cache.clone();
                async move {
                    if matches!(event.as_ref(), Event::Disconnected(_) | Event::LoggedOut(_)) {
                        session.write().await.connected = false;
                    }
                    calls.receive(event.as_ref(), &cache).await;
                    let device = client.persistence_manager().get_device_snapshot();
                    let own: Vec<_> = [device.pn.as_ref(), device.lid.as_ref()]
                        .into_iter()
                        .flatten()
                        .map(ToString::to_string)
                        .collect();
                    if events::dispatch(&cache, event, own).await.is_err() {
                        session.write().await.error = true;
                    }
                }
            })
            .build()
            .await?;
        let backend = Arc::new(Self {
            client: bot.client(),
            cache,
            session,
            writes: Mutex::new(()),
            calls,
            contacts_requested: std::sync::Mutex::new(None),
            identity_cursor: std::sync::atomic::AtomicUsize::new(0),
        });
        Ok((backend, bot))
    }

    async fn target(&self, command: &Command) -> anyhow::Result<ChatMessage> {
        let cache = self.cache.lock().await;
        let key = RoomKey::new(uuid::Uuid::nil(), &command.chat_id);
        cache
            .store
            .room(&key)
            .and_then(|room| {
                room.messages
                    .iter()
                    .find(|message| message.id == command.message_id)
            })
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("The message is not in the local history"))
    }

    fn own_user(&self) -> Option<String> {
        let device = self.client.persistence_manager().get_device_snapshot();
        device
            .pn
            .as_ref()
            .or(device.lid.as_ref())
            .map(ToString::to_string)
    }
}

fn scoped_id(own: bool, sender: &str, remote: &str) -> String {
    format!("{}|{remote}", if own { "own" } else { sender })
}

fn remote_id(id: &str) -> anyhow::Result<MessageId> {
    let (_, remote) = id
        .split_once('|')
        .ok_or_else(|| anyhow::anyhow!("Invalid message identity"))?;
    Ok(MessageId::new(remote)?)
}

#[async_trait]
impl Backend for WhatsApp {
    async fn avatar(&self, chat: &str) -> anyhow::Result<Option<Vec<u8>>> {
        self.profile_photo(chat).await
    }

    async fn call_control(
        &self,
        request: messages_protocols::calls::CallRequest,
    ) -> anyhow::Result<messages_protocols::calls::CallSnapshot> {
        self.calls.control(&self.client, request).await
    }
    async fn calls(&self) -> Vec<messages_protocols::calls::CallInvite> {
        self.calls.invites().await
    }

    async fn status(&self) -> BridgeStatus {
        self.refresh_phone_numbers().await;
        for room in &mut self.cache.lock().await.store.conversations {
            let calls = capabilities_for(&room.key.room);
            for capability in [Capability::VoiceCall, Capability::VideoCall] {
                if calls.supports(capability) {
                    room.capabilities.enable(capability);
                } else {
                    room.capabilities.disable(
                        capability,
                        "Calls are currently available in direct WhatsApp chats.",
                    );
                }
            }
        }

        {
            let mut cache = self.cache.lock().await;
            if cache.whatsapp.names_dirty {
                profiles::repair(&mut cache);
                if cache.persist().await.is_ok() {
                    cache.whatsapp.names_dirty = false;
                }
            }
        }
        let session = self.session.read().await;
        BridgeStatus {
            api_version: API_VERSION,
            protocol: "whatsapp".into(),
            authenticated: session.connected && !session.error,
            user_id: self.own_user(),
            capabilities: capabilities(),
            login_state: Some(
                if session.error {
                    "Local history could not be saved"
                } else if session.connected {
                    "Connected"
                } else if session.qr.is_some() {
                    "Scan the QR code"
                } else {
                    "Connecting"
                }
                .into(),
            ),
        }
    }

    async fn login(&self) -> Value {
        let session = self.session.read().await;
        if let Some((qr, expiry)) = &session.qr
            && *expiry > Instant::now()
        {
            return json!({"state":"qr","qr":qr,"expires_in":expiry.duration_since(Instant::now()).as_secs()});
        }
        json!({"state":if session.connected { "ready" } else { "connecting" }})
    }

    async fn authenticate(&self, _input: Value) -> anyhow::Result<()> {
        anyhow::bail!("Scan the QR code from WhatsApp Linked devices")
    }

    fn cache(&self) -> &SharedCache {
        &self.cache
    }

    async fn command(&self, operation: &str, command: Command) -> anyhow::Result<Value> {
        if operation == "attachment" {
            return self.attachment(&command).await;
        }
        let _write = self.writes.lock().await;
        if operation == "open" {
            let number = command.chat_id.trim().trim_start_matches('+');
            anyhow::ensure!(
                (7..=15).contains(&number.len())
                    && number.bytes().all(|byte| byte.is_ascii_digit()),
                "Enter a phone number with country code"
            );
            let jid = format!("{number}@s.whatsapp.net");
            let key = RoomKey::new(uuid::Uuid::nil(), &jid);
            let mut cache = self.cache.lock().await;
            if cache.store.room(&key).is_none() {
                cache
                    .store
                    .conversations
                    .push(messages_core::Conversation::empty(
                        key.clone(),
                        format!("+{number}"),
                        RoomKind::Direct,
                        Encryption::EndToEnd,
                        capabilities(),
                    ));
                cache.persist().await?;
            }
            return Ok(json!({"conversation":cache.store.room(&key)}));
        }
        let jid: Jid = command.chat_id.parse()?;
        let key = RoomKey::new(uuid::Uuid::nil(), &command.chat_id);
        if operation == "typing" {
            if command.active {
                self.client.chatstate().send_composing(&jid).await?;
            } else {
                self.client.chatstate().send_paused(&jid).await?;
            }
            return Ok(json!({"ok":true}));
        }
        if operation == "history" {
            let cache = self.cache.lock().await;
            let room = cache
                .store
                .room(&key)
                .ok_or_else(|| anyhow::anyhow!("Unknown chat"))?;
            let end = command
                .before
                .as_ref()
                .and_then(|id| room.messages.iter().position(|m| &m.id == id))
                .unwrap_or(room.messages.len());
            let start = end.saturating_sub(50);
            let next = (start > 0).then(|| room.messages[start].id.clone());
            return Ok(json!({"messages":&room.messages[start..end],"next":next}));
        }
        if operation == "send" {
            anyhow::ensure!(
                !command.body.trim().is_empty() || !command.attachments.is_empty(),
                "Empty message"
            );
            let transaction = command
                .transaction_id
                .ok_or_else(|| anyhow::anyhow!("Missing transaction ID"))?;
            {
                let cache = self.cache.lock().await;
                if let Some(existing) = cache.store.room(&key).and_then(|room| {
                    room.messages
                        .iter()
                        .find(|message| message.transaction == Some(transaction))
                }) {
                    return Ok(json!({"message_id":existing.id}));
                }
            }
            let mut context = None;
            let mut content = wa::Message::text(command.body.clone());
            if let Some(reply) = &command.reply_to {
                let cache = self.cache.lock().await;
                let target = cache
                    .store
                    .room(&key)
                    .and_then(|room| room.messages.iter().find(|message| &message.id == reply))
                    .ok_or_else(|| anyhow::anyhow!("Reply target is unavailable"))?;
                context = Some(wa::ContextInfo {
                    stanza_id: Some(remote_id(reply)?.to_string()),
                    participant: Some(target.sender.clone()),
                    quoted_message: wa::Message::text(target.body.clone()).into(),
                    ..Default::default()
                });
                content =
                    wa::Message::text_with_context(command.body.clone(), context.clone().unwrap());
            }
            if !command.attachments.is_empty() {
                return self.send_files(&jid, &command, context).await;
            }
            let result = self.client.send_message(&jid, content).await?;
            let sender = self
                .own_user()
                .ok_or_else(|| anyhow::anyhow!("Own identity unavailable"))?;
            let id = scoped_id(true, &sender, result.message_id.as_str());
            let message = ChatMessage {
                content_state: Some(messages_core::MessageContentState::Available),
                id: id.clone(),
                transaction: Some(transaction),
                sender,
                sender_name: "You".into(),
                own: true,
                body: command.body,
                timestamp: Utc::now(),
                delivery: Delivery::Sent,
                edited: false,
                deleted: false,
                reply_to: command.reply_to,
                reactions: Vec::new(),
                attachments: Vec::new(),
            };
            let mut cache = self.cache.lock().await;
            events::merge(
                &mut cache,
                &command.chat_id,
                &command.chat_id,
                if command.chat_id.ends_with("@g.us") {
                    RoomKind::Group
                } else {
                    RoomKind::Direct
                },
                message,
            )?;
            cache.persist().await?;
            return Ok(json!({"message_id":id}));
        }
        let target = self.target(&command).await?;
        let sender: Jid = target.sender.parse()?;
        let reference = MessageRef::new(&jid, remote_id(&target.id)?, Some(&sender), target.own)?;
        match operation {
            "read" => {
                if !target.own {
                    self.client.mark_message_read(&reference).await?;
                }
                return Ok(json!({"ok":true}));
            }
            "react" => {
                self.client
                    .send_reaction(&reference, if command.remove { "" } else { &command.emoji })
                    .await?;
            }
            "edit" => {
                anyhow::ensure!(target.own, "Cannot edit another sender's message");
                self.client
                    .edit_message(EditRequest::new(
                        reference,
                        wa::Message::text(command.body.clone()),
                    ))
                    .await?;
            }
            "delete" => {
                anyhow::ensure!(target.own, "Cannot delete another sender's message");
                self.client.revoke_message(&reference).await?;
            }
            _ => anyhow::bail!("Unsupported operation"),
        }
        let mut cache = self.cache.lock().await;
        match operation {
            "edit" => events::edit(
                &mut cache,
                &command.chat_id,
                &target.id,
                &command.body,
                Utc::now().timestamp_millis(),
            )?,
            "delete" => events::delete(&mut cache, &command.chat_id, &target.id)?,
            "react" => events::annotate(
                &mut cache,
                &command.chat_id,
                &target.id,
                "own",
                if command.remove { "" } else { &command.emoji },
                Utc::now().timestamp_millis(),
                true,
            )?,
            _ => {}
        }
        cache.persist().await?;
        Ok(json!({"ok":true}))
    }
}

fn capabilities_for(chat: &str) -> Capabilities {
    let mut caps = capabilities();
    if chat.ends_with("@g.us") || (!chat.ends_with("@s.whatsapp.net") && !chat.ends_with("@lid")) {
        caps.disable(
            Capability::VoiceCall,
            "Calls are currently available in direct WhatsApp chats.",
        );
        caps.disable(
            Capability::VideoCall,
            "Calls are currently available in direct WhatsApp chats.",
        );
    }
    caps
}
