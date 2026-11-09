//! Matrix client-server transport; encrypted rooms require the SDK host.

use std::collections::HashSet;
use std::sync::Mutex;

use async_trait::async_trait;
use messages_core::{
    Capabilities, Capability, ChatMessage, Conversation, Encryption, RoomKey, RoomKind, SendRequest,
};
use reqwest::Method;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{Adapter, AdapterError, ConnectionConfig, MessageAction, SyncBatch, http::Http};

pub struct Matrix {
    config: ConnectionConfig,
    pub(crate) http: Http,
    encrypted: Mutex<HashSet<String>>,
    timeline: Mutex<crate::matrix_timeline::MatrixTimeline>,
}

impl Matrix {
    pub fn new(config: ConnectionConfig, token: Option<String>) -> Result<Self, AdapterError> {
        let token = token
            .filter(|s| !s.is_empty())
            .ok_or(AdapterError::Authentication)?;
        let http = Http::new(&config.endpoint, Some(format!("Bearer {token}")))?;
        Ok(Self {
            config,
            http,
            encrypted: Mutex::new(HashSet::new()),
            timeline: Mutex::default(),
        })
    }

    fn encrypted_room(&self, room: &str) -> Result<bool, AdapterError> {
        Ok(self
            .encrypted
            .lock()
            .map_err(|_| AdapterError::InvalidData("room state unavailable".into()))?
            .contains(room))
    }

    pub(crate) async fn require_plaintext(&self, room: &RoomKey) -> Result<(), AdapterError> {
        self.validate_room(room)?;
        // A send may precede initial sync, and a room can enable encryption
        // between syncs. Only an authoritative absent encryption state permits
        // this REST-only adapter to put plaintext on the wire.
        if !self.encrypted_room(&room.room)? {
            match self
                .http
                .json(
                    Method::GET,
                    &[
                        "_matrix",
                        "client",
                        "v3",
                        "rooms",
                        &room.room,
                        "state",
                        "m.room.encryption",
                    ],
                    &[],
                    None,
                )
                .await
            {
                Err(AdapterError::Http(404)) => return Ok(()),
                Err(error) => return Err(error),
                Ok(_) => {
                    self.encrypted
                        .lock()
                        .map_err(|_| AdapterError::InvalidData("room state unavailable".into()))?
                        .insert(room.room.clone());
                }
            }
        }
        if self.encrypted_room(&room.room)? {
            Err(AdapterError::Unsupported("This room is encrypted. Connect through the Matrix encryption bridge to send and read it.".into()))
        } else {
            Ok(())
        }
    }

    pub(crate) async fn event(
        &self,
        room: &RoomKey,
        event: &str,
        transaction: &str,
        content: Value,
    ) -> Result<String, AdapterError> {
        self.require_plaintext(room).await?;
        let response = self
            .http
            .json(
                Method::PUT,
                &[
                    "_matrix",
                    "client",
                    "v3",
                    "rooms",
                    &room.room,
                    "send",
                    event,
                    transaction,
                ],
                &[],
                Some(content.clone()),
            )
            .await?;
        let id = string(&response, "event_id")?;
        self.record_event(&room.room, &id, event, content)?;
        Ok(id)
    }

    fn record_event(
        &self,
        room: &str,
        id: &str,
        kind: &str,
        content: Value,
    ) -> Result<(), AdapterError> {
        self.parse_events(
            room,
            [
                &json!({"event_id":id,"type":kind,"sender":self.config.user_id,
            "origin_server_ts":chrono::Utc::now().timestamp_millis(),"content":content}),
            ],
        )?;
        Ok(())
    }

    async fn redact(&self, room: &RoomKey, target: &str) -> Result<(), AdapterError> {
        let transaction = Uuid::new_v4().to_string();
        let response = self
            .http
            .json(
                Method::PUT,
                &[
                    "_matrix",
                    "client",
                    "v3",
                    "rooms",
                    &room.room,
                    "redact",
                    target,
                    &transaction,
                ],
                &[],
                Some(json!({})),
            )
            .await?;
        self.record_event(
            &room.room,
            &string(&response, "event_id")?,
            "m.room.redaction",
            json!({"redacts":target}),
        )
    }

    fn parse_events<'a>(
        &self,
        room: &str,
        events: impl IntoIterator<Item = &'a Value>,
    ) -> Result<Vec<ChatMessage>, AdapterError> {
        let mut timeline = self
            .timeline
            .lock()
            .map_err(|_| AdapterError::InvalidData("Timeline state unavailable".into()))?;
        let mut updated = std::collections::BTreeMap::new();
        for event in events {
            if let Some(message) = timeline.apply(room, event, &self.config.user_id)? {
                updated.insert(message.id.clone(), message);
            }
        }
        let mut messages: Vec<_> = updated.into_values().collect();
        messages.sort_by_key(|message| message.timestamp);
        Ok(messages)
    }

    fn parse_room(&self, id: &str, data: &Value) -> Result<Conversation, AdapterError> {
        let mut title = id.to_string();
        let mut encrypted = self.encrypted_room(id)?;
        let mut members = Vec::new();
        let events = data["state"]["events"]
            .as_array()
            .into_iter()
            .flatten()
            .chain(data["timeline"]["events"].as_array().into_iter().flatten());
        for event in events {
            match event["type"].as_str().unwrap_or("") {
                "m.room.name" => {
                    if let Some(name) = event["content"]["name"].as_str() {
                        title = name.into();
                    }
                }
                "m.room.encryption" => encrypted = true,
                "m.room.member" if event["content"]["membership"] == "join" => {
                    if let Some(name) = event["content"]["displayname"]
                        .as_str()
                        .or(event["state_key"].as_str())
                    {
                        members.push(name.to_owned());
                    }
                }
                _ => {}
            }
        }
        if encrypted {
            self.encrypted
                .lock()
                .map_err(|_| AdapterError::InvalidData("room state unavailable".into()))?
                .insert(id.into());
        }
        let mut capabilities = self.capabilities();
        if encrypted {
            for capability in Capability::ALL {
                capabilities.disable(
                    capability,
                    "Connect the Matrix encryption bridge to use this encrypted room.",
                );
            }
        }
        Ok(Conversation {
            key: RoomKey::new(self.config.account, id),
            title,
            subtitle: if encrypted {
                "Matrix · Encrypted room".into()
            } else {
                "Matrix".into()
            },
            kind: RoomKind::Group,
            members,
            unread: data["unread_notifications"]["notification_count"]
                .as_u64()
                .unwrap_or(0)
                .min(u32::MAX as u64) as u32,
            pinned: false,
            muted: false,
            muted_until: None,
            avatar_revision: 0,
            group_name: None,
            parent_room: None,
            archived: false,
            encryption: if encrypted {
                Encryption::Unavailable
            } else {
                Encryption::Transport
            },
            capabilities,
            messages: self.parse_events(
                id,
                data["timeline"]["events"].as_array().into_iter().flatten(),
            )?,
            draft: String::new(),
            draft_attachments: Vec::new(),
            reply_to: None,
            has_older: data["timeline"]["limited"].as_bool().unwrap_or(false),
            pagination: data["timeline"]["prev_batch"].as_str().map(str::to_owned),
        })
    }
}

#[async_trait]
impl Adapter for Matrix {
    fn config(&self) -> &ConnectionConfig {
        &self.config
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities::new([
            Capability::StartConversation,
            Capability::SendText,
            Capability::AttachFiles,
            Capability::Reply,
            Capability::React,
            Capability::EditOwn,
            Capability::DeleteOwn,
            Capability::ReadReceipts,
            Capability::Typing,
        ])
    }
    fn idempotent_sends(&self) -> bool {
        true
    }

    async fn sync(&self, cursor: Option<&str>) -> Result<SyncBatch, AdapterError> {
        let mut query = vec![("timeout", "1000")];
        if let Some(cursor) = cursor {
            query.push(("since", cursor));
        }
        let value = self
            .http
            .json(
                Method::GET,
                &["_matrix", "client", "v3", "sync"],
                &query,
                None,
            )
            .await?;
        let next = string(&value, "next_batch")?;
        let mut batch = SyncBatch {
            cursor: Some(next),
            ..SyncBatch::default()
        };
        if let Some(rooms) = value["rooms"]["join"].as_object() {
            for (id, data) in rooms {
                batch.conversations.push(self.parse_room(id, data)?);
            }
        }
        Ok(batch)
    }

    async fn history(
        &self,
        room: &RoomKey,
        before: Option<&str>,
    ) -> Result<Vec<ChatMessage>, AdapterError> {
        Ok(self.history_page(room, before).await?.messages)
    }

    async fn history_page(
        &self,
        room: &RoomKey,
        before: Option<&str>,
    ) -> Result<crate::HistoryPage, AdapterError> {
        self.require_plaintext(room).await?;
        let mut query = vec![("dir", "b"), ("limit", "50")];
        if let Some(before) = before {
            query.push(("from", before));
        }
        let value = self
            .http
            .json(
                Method::GET,
                &["_matrix", "client", "v3", "rooms", &room.room, "messages"],
                &query,
                None,
            )
            .await?;
        let events = value["chunk"]
            .as_array()
            .ok_or_else(|| AdapterError::InvalidData("missing message history".into()))?;
        Ok(crate::HistoryPage {
            messages: self.parse_events(&room.room, events)?,
            next: value["end"].as_str().map(str::to_owned),
        })
    }

    async fn start_conversation(&self, recipient: &str) -> Result<Conversation, AdapterError> {
        self.require(Capability::StartConversation)?;
        let recipient = recipient.trim();
        if recipient.len() > 255
            || !recipient.contains(':')
            || recipient.chars().any(char::is_whitespace)
        {
            return Err(AdapterError::Configuration(
                "Enter a Matrix user ID, room ID or alias including its server.".into(),
            ));
        }
        let value = if recipient.starts_with('@') {
            self.http.json(Method::POST, &["_matrix","client","v3","createRoom"], &[],
                Some(json!({"is_direct":true,"invite":[recipient],"preset":"private_chat","visibility":"private"}))).await?
        } else if recipient.starts_with('!') || recipient.starts_with('#') {
            self.http
                .json(
                    Method::POST,
                    &["_matrix", "client", "v3", "join", recipient],
                    &[],
                    Some(json!({})),
                )
                .await?
        } else {
            return Err(AdapterError::Configuration(
                "Matrix IDs start with @, ! or #.".into(),
            ));
        };
        let id = string(&value, "room_id")?;
        let mut room = self.parse_room(&id, &json!({}))?;
        room.title = recipient.into();
        room.kind = if recipient.starts_with('@') {
            RoomKind::Direct
        } else {
            RoomKind::Group
        };
        Ok(room)
    }

    async fn send(&self, request: &SendRequest) -> Result<String, AdapterError> {
        self.validate_send(request)?;
        self.require(Capability::SendText)?;
        if !request.attachments.is_empty() {
            return self.send_files(request).await;
        }
        let mut content = json!({"msgtype":"m.text", "body":request.body});
        if let Some(reply) = &request.reply_to {
            content["m.relates_to"] = json!({"m.in_reply_to":{"event_id":reply}});
        }
        self.event(
            &request.room,
            "m.room.message",
            &request.transaction.to_string(),
            content,
        )
        .await
    }

    async fn attachment(
        &self,
        room: &RoomKey,
        file: &messages_core::Attachment,
    ) -> Result<Vec<u8>, AdapterError> {
        self.download_file(room, file).await
    }

    async fn action(&self, room: &RoomKey, action: MessageAction) -> Result<(), AdapterError> {
        self.require(action.capability())?;
        self.require_plaintext(room).await?;
        let transaction = Uuid::new_v4().to_string();
        match action {
            MessageAction::React {
                message,
                emoji,
                remove: false,
            } => {
                self.event(room, "m.reaction", &transaction,
                    json!({"m.relates_to":{"rel_type":"m.annotation", "event_id":message.id, "key":emoji}})).await?;
            }
            MessageAction::React {
                message,
                emoji,
                remove: true,
            } => {
                let ids = self
                    .timeline
                    .lock()
                    .map_err(|_| AdapterError::InvalidData("Timeline state unavailable".into()))?
                    .own_reaction_ids(&room.room, &message.id, &emoji, &self.config.user_id);
                for id in ids {
                    self.redact(room, &id).await?;
                }
            }
            MessageAction::Edit { message, body } => {
                if !message.own {
                    return Err(AdapterError::Unsupported(
                        "You can only edit your own messages.".into(),
                    ));
                }
                self.event(
                    room,
                    "m.room.message",
                    &transaction,
                    json!({"msgtype":"m.text", "body":format!("* {body}"),
                    "m.new_content":{"msgtype":"m.text", "body":body},
                    "m.relates_to":{"rel_type":"m.replace", "event_id":message.id}}),
                )
                .await?;
            }
            MessageAction::Delete { message } => {
                if !message.own {
                    return Err(AdapterError::Unsupported(
                        "You can only delete your own messages.".into(),
                    ));
                }
                self.redact(room, &message.id).await?;
            }
            MessageAction::Read { message } => {
                self.http
                    .json(
                        Method::POST,
                        &[
                            "_matrix", "client", "v3", "rooms", &room.room, "receipt", "m.read",
                            &message,
                        ],
                        &[],
                        Some(json!({})),
                    )
                    .await?;
            }
            MessageAction::Typing { active } => {
                self.http
                    .json(
                        Method::PUT,
                        &[
                            "_matrix",
                            "client",
                            "v3",
                            "rooms",
                            &room.room,
                            "typing",
                            &self.config.user_id,
                        ],
                        &[],
                        Some(json!({"typing":active, "timeout":5000})),
                    )
                    .await?;
            }
        }
        Ok(())
    }
}

pub(crate) fn string(value: &Value, key: &str) -> Result<String, AdapterError> {
    value[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| AdapterError::InvalidData(format!("missing {key}")))
}
