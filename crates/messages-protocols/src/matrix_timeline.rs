//! Matrix event identity and relation reduction, shared by REST and SDK hosts.
//! SDK callers supply decrypted events; ciphertext stays typed until keys arrive.

use std::collections::{BTreeMap, BTreeSet};

use messages_core::{ChatMessage, Delivery, Reaction};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::AdapterError;

#[derive(Default, Serialize, Deserialize)]
pub struct MatrixTimeline {
    rooms: BTreeMap<String, RoomEvents>,
}

#[derive(Default, Serialize, Deserialize)]
struct RoomEvents {
    messages: BTreeMap<String, Original>,
    edits: BTreeMap<String, Edit>,
    reactions: BTreeMap<String, Annotation>,
    redacted: BTreeSet<String>,
}

#[derive(Serialize, Deserialize)]
struct Original {
    message: ChatMessage,
    encrypted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ciphertext: Option<Value>,
}

#[derive(Serialize, Deserialize)]
struct Edit {
    target: String,
    sender: String,
    body: String,
    timestamp: i64,
}

#[derive(Serialize, Deserialize)]
struct Annotation {
    target: String,
    sender: String,
    emoji: String,
}

impl MatrixTimeline {
    pub fn apply(
        &mut self,
        room: &str,
        event: &Value,
        own: &str,
    ) -> Result<Option<ChatMessage>, AdapterError> {
        let kind = event["type"].as_str().unwrap_or("");
        if !matches!(
            kind,
            "m.room.message" | "m.room.encrypted" | "m.reaction" | "m.room.redaction"
        ) {
            return Ok(None);
        }
        let id = field(event, "event_id")?;
        let events = self.rooms.entry(room.to_owned()).or_default();
        if kind == "m.room.encrypted"
            && (events.edits.contains_key(id) || events.reactions.contains_key(id))
        {
            return Ok(None);
        }
        let content = &event["content"];
        if event["unsigned"]["redacted_because"].is_object() {
            let target = events.redact(id);
            if target != id || kind == "m.reaction" {
                return Ok(events.materialize(&target, own));
            }
        }
        let target = if kind == "m.room.redaction" {
            let redacted = content["redacts"]
                .as_str()
                .or(event["redacts"].as_str())
                .ok_or_else(|| invalid("Missing redaction target"))?;
            events.redact(redacted)
        } else if kind == "m.reaction" {
            if events.messages.get(id).is_some_and(|old| old.encrypted) {
                events.messages.remove(id);
            }
            if content["m.relates_to"]["rel_type"] != "m.annotation" {
                return Ok(None);
            }
            let relation = &content["m.relates_to"];
            let target = field(relation, "event_id")?.to_owned();
            events.reactions.entry(id.to_owned()).or_insert(Annotation {
                target: target.clone(),
                sender: field(event, "sender")?.into(),
                emoji: field(relation, "key")?.into(),
            });
            target
        } else if content["m.relates_to"]["rel_type"] == "m.replace" {
            if events.messages.get(id).is_some_and(|old| old.encrypted) {
                events.messages.remove(id);
            }
            let target = field(&content["m.relates_to"], "event_id")?.to_owned();
            let body = field(&content["m.new_content"], "body")?;
            events.edits.entry(id.to_owned()).or_insert(Edit {
                target: target.clone(),
                sender: field(event, "sender")?.into(),
                body: if events.redacted.contains(id) || events.redacted.contains(&target) {
                    String::new()
                } else {
                    body.into()
                },
                timestamp: event["origin_server_ts"]
                    .as_i64()
                    .ok_or_else(|| invalid("Missing edit timestamp"))?,
            });
            target
        } else {
            let encrypted = kind == "m.room.encrypted";
            let sender = field(event, "sender")?;
            let timestamp = event["origin_server_ts"]
                .as_i64()
                .and_then(chrono::DateTime::from_timestamp_millis)
                .ok_or_else(|| invalid("Invalid Matrix timestamp"))?;
            let deleted = events.redacted.contains(id);
            let body = if deleted || encrypted {
                ""
            } else {
                content["body"].as_str().unwrap_or("")
            };
            let message = ChatMessage {
                content_state: Some(if encrypted {
                    messages_core::MessageContentState::WaitingForKeys
                } else {
                    messages_core::MessageContentState::Available
                }),
                id: id.into(),
                transaction: event["unsigned"]["transaction_id"]
                    .as_str()
                    .and_then(|id| uuid::Uuid::parse_str(id).ok()),
                sender: sender.into(),
                sender_name: sender
                    .trim_start_matches('@')
                    .split(':')
                    .next()
                    .unwrap_or(sender)
                    .into(),
                own: sender == own,
                body: body.into(),
                timestamp,
                delivery: if sender == own {
                    Delivery::Sent
                } else {
                    Delivery::Received
                },
                edited: false,
                deleted,
                reply_to: content["m.relates_to"]["m.in_reply_to"]["event_id"]
                    .as_str()
                    .map(str::to_owned),
                reactions: Vec::new(),
                attachments: if deleted || encrypted {
                    Vec::new()
                } else {
                    matrix_attachments(id, content)
                },
            };
            // Original events are immutable. Only decryption may replace an
            // existing payload; replay cannot revert edits or reintroduce text.
            if events
                .messages
                .get(id)
                .is_none_or(|old| old.encrypted && !encrypted)
            {
                events.messages.insert(
                    id.into(),
                    Original {
                        message,
                        encrypted,
                        ciphertext: encrypted.then(|| event.clone()),
                    },
                );
            } else if encrypted
                && let Some(old) = events.messages.get_mut(id).filter(|old| old.encrypted)
            {
                old.ciphertext = Some(event.clone());
            }
            id.into()
        };
        Ok(events.materialize(&target, own))
    }

    /// Pending encrypted events, including old caches that need a server refetch.
    /// Redacted events are excluded even when their ciphertext is retained.
    pub fn pending_decryption(&self) -> Vec<(String, String, Option<Value>)> {
        self.rooms
            .iter()
            .flat_map(|(room, events)| {
                events
                    .messages
                    .iter()
                    .filter(|(id, event)| event.encrypted && !events.redacted.contains(*id))
                    .map(move |(id, event)| (room.clone(), id.clone(), event.ciphertext.clone()))
            })
            .collect()
    }

    pub fn awaiting_keys(&self, room: &str, id: &str) -> bool {
        self.rooms
            .get(room)
            .and_then(|events| events.messages.get(id))
            .is_some_and(|event| event.encrypted)
    }

    pub fn own_reaction_ids(
        &self,
        room: &str,
        target: &str,
        emoji: &str,
        own: &str,
    ) -> Vec<String> {
        self.rooms
            .get(room)
            .into_iter()
            .flat_map(|events| {
                events
                    .reactions
                    .iter()
                    .filter(|(id, reaction)| {
                        reaction.target == target
                            && reaction.emoji == emoji
                            && reaction.sender == own
                            && !events.redacted.contains(*id)
                    })
                    .map(|(id, _)| id.clone())
            })
            .collect()
    }
}

impl RoomEvents {
    fn redact(&mut self, id: &str) -> String {
        self.redacted.insert(id.to_owned());
        if let Some(original) = self.messages.get_mut(id) {
            original.ciphertext = None;
            original.message.body.clear();
            original.message.attachments.clear();
            original.message.reply_to = None;
            original.message.deleted = true;
        }
        for edit in self.edits.values_mut().filter(|edit| edit.target == id) {
            edit.body.clear();
        }
        if let Some(edit) = self.edits.get_mut(id) {
            edit.body.clear();
            return edit.target.clone();
        }
        if let Some(reaction) = self.reactions.get(id) {
            return reaction.target.clone();
        }
        id.to_owned()
    }

    fn materialize(&self, id: &str, own: &str) -> Option<ChatMessage> {
        let original = self.messages.get(id)?;
        let mut message = original.message.clone();
        message.content_state = Some(if original.encrypted {
            messages_core::MessageContentState::WaitingForKeys
        } else {
            messages_core::MessageContentState::Available
        });
        if original.encrypted {
            message.body.clear();
        }
        if self.redacted.contains(id) {
            message.body.clear();
            message.deleted = true;
            message.reply_to = None;
            message.reactions.clear();
            message.attachments.clear();
            return Some(message);
        }
        if let Some((_, edit)) = self
            .edits
            .iter()
            .filter(|(event_id, edit)| {
                edit.target == id
                    && edit.sender == message.sender
                    && !self.redacted.contains(*event_id)
            })
            .max_by_key(|(event_id, edit)| (edit.timestamp, *event_id))
        {
            message.body = edit.body.clone();
            message.edited = true;
        }
        let mut reactions = BTreeMap::<&str, BTreeSet<&str>>::new();
        for (event_id, reaction) in &self.reactions {
            if reaction.target == id && !self.redacted.contains(event_id) {
                reactions
                    .entry(&reaction.emoji)
                    .or_default()
                    .insert(&reaction.sender);
            }
        }
        message.reactions = reactions
            .into_iter()
            .map(|(emoji, senders)| Reaction {
                emoji: emoji.into(),
                count: senders.len().min(u32::MAX as usize) as u32,
                own: senders.contains(own),
            })
            .collect();
        Some(message)
    }
}

fn field<'a>(value: &'a Value, name: &str) -> Result<&'a str, AdapterError> {
    value[name]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid("Missing Matrix event field"))
}

fn invalid(message: &str) -> AdapterError {
    AdapterError::InvalidData(message.into())
}

fn matrix_attachments(id: &str, content: &Value) -> Vec<messages_core::Attachment> {
    if !matches!(
        content["msgtype"].as_str(),
        Some("m.image" | "m.file" | "m.video" | "m.audio")
    ) {
        return Vec::new();
    }
    let source = if content["file"].is_object() {
        Some(content["file"].to_string())
    } else {
        content["url"].as_str().map(str::to_owned)
    };
    vec![messages_core::Attachment {
        id: id.into(),
        name: content["filename"]
            .as_str()
            .or(content["body"].as_str())
            .unwrap_or("Attachment")
            .into(),
        mime: content["info"]["mimetype"]
            .as_str()
            .unwrap_or(match content["msgtype"].as_str() {
                Some("m.image") => "image/jpeg",
                Some("m.video") => "video/mp4",
                Some("m.audio") => "audio/ogg",
                _ => "application/octet-stream",
            })
            .into(),
        bytes: content["info"]["size"].as_u64().unwrap_or(0),
        remote_url: source,
    }]
}
