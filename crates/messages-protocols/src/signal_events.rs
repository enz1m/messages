use messages_core::{ChangeKind, Delivery, MessageChange, RoomKey};
use serde_json::Value;

use crate::{SyncBatch, TypingUpdate, signal::Signal};

fn address<'a>(value: &'a Value, fields: &[&str]) -> Option<&'a str> {
    fields
        .iter()
        .find_map(|field| value[*field].as_str().filter(|value| !value.is_empty()))
}

impl Signal {
    pub(crate) fn receive_changes(&self, value: &Value, batch: &mut SyncBatch) {
        let Ok(user) = self.active_user() else {
            return;
        };
        let params = value.get("params").unwrap_or(value);
        let params = params.get("result").unwrap_or(params);
        if params["account"]
            .as_str()
            .is_some_and(|account| account != user)
        {
            return;
        }
        let envelope = &params["envelope"];
        let source = self.address(envelope, "sourceNumber", "sourceUuid", "source");
        let source = source.as_deref();
        for read in envelope["syncMessage"]["readMessages"]
            .as_array()
            .into_iter()
            .flatten()
        {
            if let (Some(sender), Some(timestamp)) = (
                self.address(read, "senderNumber", "senderUuid", "sender"),
                read["timestamp"].as_i64(),
            ) {
                batch.device_reads.push(messages_core::DeviceRead {
                    account: self.config.account,
                    sender,
                    timestamp,
                });
            }
        }
        let sent = &envelope["syncMessage"]["sentMessage"];
        let own = sent.is_object();
        let actor = if own {
            user.as_str()
        } else {
            match source {
                Some(source) => source,
                None => return,
            }
        };
        let edit = if own {
            &sent["editMessage"]
        } else {
            &envelope["editMessage"]
        };
        let data = if edit.is_object() {
            &edit["dataMessage"]
        } else if own {
            sent
        } else {
            &envelope["dataMessage"]
        };
        let destination = self.address(sent, "destinationNumber", "destinationUuid", "destination");
        let room = data["groupInfo"]["groupId"]
            .as_str()
            .map(|group| format!("group:{group}"))
            .or_else(|| {
                if own {
                    destination
                } else {
                    source.map(str::to_owned)
                }
            });
        if let (Some(id), Some(name)) = (
            data["groupInfo"]["groupId"].as_str(),
            data["groupInfo"]["groupName"].as_str(),
        ) {
            batch.conversations.push(self.conversation(
                &format!("group:{id}"),
                name,
                messages_core::RoomKind::Group,
            ));
        }
        let version = data["timestamp"]
            .as_i64()
            .or(envelope["timestamp"].as_i64())
            .unwrap_or(0);
        if own
            && edit.is_null()
            && data["reaction"].is_null()
            && data["remoteDelete"].is_null()
            && version > 0
            && let Some(room) = room.clone()
        {
            batch
                .read_through
                .push((RoomKey::new(self.config.account, room), version));
        }
        let mut push = |room: Option<String>, author: &str, timestamp: i64, kind: ChangeKind| {
            if timestamp > 0 {
                batch.changes.push(MessageChange {
                    account: self.config.account,
                    room,
                    message: format!("{author}:{timestamp}"),
                    kind,
                });
            }
        };
        if !(data["viewOnce"] == true
            || data["expiresInSeconds"]
                .as_i64()
                .is_some_and(|seconds| seconds > 0))
            && let (Some(target), Some(body), Some(room)) = (
                edit["targetSentTimestamp"].as_i64(),
                data["message"].as_str(),
                room.clone(),
            )
        {
            push(
                Some(room),
                actor,
                target,
                ChangeKind::Edit {
                    timestamp: version,
                    body: body.into(),
                },
            );
        }
        let reaction = &data["reaction"];
        if let (Some(target), Some(author), Some(emoji), Some(room)) = (
            reaction["targetSentTimestamp"].as_i64(),
            address(
                reaction,
                &["targetAuthorNumber", "targetAuthorUuid", "targetAuthor"],
            ),
            reaction["emoji"].as_str(),
            room.clone(),
        ) {
            push(
                Some(room),
                &self.resolve(author),
                target,
                ChangeKind::Reaction {
                    timestamp: version,
                    actor: actor.into(),
                    emoji: emoji.into(),
                    remove: reaction["isRemove"].as_bool().unwrap_or(false),
                    own,
                },
            );
        }
        if let (Some(target), Some(room)) = (
            data["remoteDelete"]["targetTimestamp"]
                .as_i64()
                .or(data["remoteDelete"]["timestamp"].as_i64()),
            room,
        ) {
            push(Some(room), actor, target, ChangeKind::Delete);
        }
        let receipt = &envelope["receiptMessage"];
        if receipt["isDelivery"] == true || receipt["isRead"] == true || receipt["isViewed"] == true
        {
            let delivery = if receipt["isRead"] == true || receipt["isViewed"] == true {
                Delivery::Read
            } else {
                Delivery::Delivered
            };
            for timestamp in receipt["timestamps"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_i64)
            {
                push(
                    None,
                    &user,
                    timestamp,
                    ChangeKind::Receipt(delivery.clone()),
                );
            }
        }
        let typing = &envelope["typingMessage"];
        if let (Some(action), Some(source)) = (typing["action"].as_str(), source) {
            let room = typing["groupId"]
                .as_str()
                .map(|id| format!("group:{id}"))
                .unwrap_or_else(|| source.into());
            batch.typing.push(TypingUpdate {
                room: RoomKey::new(self.config.account, room),
                actor: source.into(),
                name: envelope["sourceName"].as_str().unwrap_or(source).into(),
                active: action == "STARTED",
            });
        }
    }
}
