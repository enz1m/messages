//! signal-cli HTTP JSON-RPC plus resumable server-sent events. Native Signal
//! cryptography and linked-device sessions remain in signal-cli/libsignal.

use std::time::Duration;

use async_trait::async_trait;
use chrono::DateTime;
use messages_core::{
    Capabilities, Capability, ChatMessage, Conversation, Delivery, Encryption, RoomKey, RoomKind,
    SendRequest,
};
use reqwest::Method;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{Adapter, AdapterError, ConnectionConfig, MessageAction, SyncBatch, http::Http};

pub struct Signal {
    pub(crate) config: ConnectionConfig,
    pub(crate) http: Http,
    pub(crate) login_gate: tokio::sync::Mutex<()>,
    pub(crate) auth: std::sync::Mutex<crate::signal_login::SignalAuth>,
    events: tokio::sync::Mutex<crate::sse::Connection>,
    pub(crate) identities: std::sync::Mutex<std::collections::HashMap<String, String>>,
}

impl Signal {
    pub fn new(config: ConnectionConfig, token: Option<String>) -> Result<Self, AdapterError> {
        let http = Http::new(
            &config.endpoint,
            token.map(|token| format!("Bearer {token}")),
        )?;
        Ok(Self {
            config,
            http,
            auth: crate::signal_login::auth(),
            login_gate: tokio::sync::Mutex::new(()),
            events: tokio::sync::Mutex::default(),
            identities: std::sync::Mutex::default(),
        })
    }

    pub(crate) async fn rpc(&self, method: &str, mut params: Value) -> Result<Value, AdapterError> {
        params["account"] = json!(self.active_user()?);
        let id = Uuid::new_v4().to_string();
        let result = self
            .rpc_response(method, params, &id, 8 * 1024 * 1024)
            .await?;
        if result["id"] != id {
            return Err(AdapterError::InvalidData(
                "JSON-RPC response identity mismatch".into(),
            ));
        }
        if !result["error"].is_null() {
            return Err(AdapterError::InvalidData(format!(
                "signal-cli rejected {method} (code {})",
                result["error"]["code"].as_i64().unwrap_or(0)
            )));
        }
        result
            .get("result")
            .cloned()
            .ok_or_else(|| AdapterError::InvalidData("missing JSON-RPC result".into()))
    }

    fn recipient(room: &RoomKey) -> Value {
        if let Some(group) = room.room.strip_prefix("group:") {
            json!({"groupId":group})
        } else {
            json!({"recipient":[room.room]})
        }
    }

    fn message_identity(id: &str) -> Result<(&str, i64), AdapterError> {
        let (author, timestamp) = id
            .rsplit_once(':')
            .filter(|(author, _)| !author.is_empty())
            .ok_or_else(|| AdapterError::InvalidData("invalid Signal message identity".into()))?;
        let timestamp = timestamp
            .parse::<i64>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(|| AdapterError::InvalidData("invalid Signal message timestamp".into()))?;
        Ok((author, timestamp))
    }

    pub(crate) fn conversation(&self, id: &str, title: &str, kind: RoomKind) -> Conversation {
        Conversation {
            key: RoomKey::new(self.config.account, id),
            title: title.into(),
            subtitle: "Signal".into(),
            kind,
            members: Vec::new(),
            unread: 0,
            pinned: false,
            muted: false,
            muted_until: None,
            avatar_revision: 0,
            group_name: None,
            parent_room: None,
            archived: false,
            encryption: Encryption::EndToEnd,
            capabilities: self.capabilities(),
            messages: Vec::new(),
            draft: String::new(),
            draft_attachments: Vec::new(),
            reply_to: None,
            has_older: false,
            pagination: None,
        }
    }

    fn receive(&self, value: &Value) -> Option<(RoomKey, ChatMessage)> {
        let user = self.active_user().ok()?;
        let params = value.get("params").unwrap_or(value);
        let params = params.get("result").unwrap_or(params);
        if params["account"]
            .as_str()
            .is_some_and(|account| account != user)
        {
            return None;
        }
        let envelope = &params["envelope"];
        let sent = &envelope["syncMessage"]["sentMessage"];
        let own = sent.is_object();
        let data = if own { sent } else { &envelope["dataMessage"] };
        if data["reaction"].is_object()
            || data["remoteDelete"].is_object()
            || data["editMessage"].is_object()
        {
            return None;
        }
        let attachments = crate::signal_media::attachments(data);
        let protected = data["viewOnce"] == true
            || data["expiresInSeconds"]
                .as_i64()
                .is_some_and(|seconds| seconds > 0);
        let attachments = if protected { Vec::new() } else { attachments };
        let body = if protected {
            "Protected message — open it in Signal."
        } else {
            data["message"].as_str().unwrap_or("")
        };
        if data["isExpirationUpdate"] == true || (body.is_empty() && attachments.is_empty()) {
            return None;
        }
        let timestamp = data["timestamp"]
            .as_i64()
            .or(envelope["timestamp"].as_i64())?;
        let sender = if own {
            user.clone()
        } else {
            self.address(envelope, "sourceNumber", "sourceUuid", "source")?
        };
        let group = data["groupInfo"]["groupId"].as_str();
        let destination = self.address(sent, "destinationNumber", "destinationUuid", "destination");
        let room = group.map(|group| format!("group:{group}")).or_else(|| {
            if own {
                destination
            } else {
                Some(sender.clone())
            }
        })?;
        Some((
            RoomKey::new(self.config.account, room),
            ChatMessage {
                content_state: Some(messages_core::MessageContentState::Available),
                id: format!("{sender}:{timestamp}"),
                transaction: None,
                sender: sender.clone(),
                sender_name: if own {
                    "You".into()
                } else {
                    envelope["sourceName"].as_str().unwrap_or(&sender).into()
                },
                own,
                body: body.into(),
                timestamp: DateTime::from_timestamp_millis(timestamp)?,
                delivery: if own {
                    Delivery::Sent
                } else {
                    Delivery::Received
                },
                edited: false,
                deleted: false,
                reply_to: (!protected)
                    .then(|| data["quote"]["id"].as_i64())
                    .flatten()
                    .map(|id| {
                        format!(
                            "{}:{id}",
                            self.address(&data["quote"], "authorNumber", "authorUuid", "author")
                                .unwrap_or_else(|| sender.clone())
                        )
                    }),
                reactions: Vec::new(),
                attachments,
            },
        ))
    }

    async fn receive_batch(
        &self,
        cursor: Option<&str>,
        batch: &mut SyncBatch,
    ) -> Result<(), AdapterError> {
        let mut connection = self.events.lock().await;
        if connection.response.is_none() {
            let mut request = self
                .http
                .request(Method::GET, self.http.url(&["api", "v1", "events"])?)
                .header(reqwest::header::ACCEPT, "text/event-stream")
                .timeout(Duration::from_secs(3600));
            // An absent header means live events only; zero requests the daemon's buffered backlog.
            request = request.header("Last-Event-ID", cursor.unwrap_or("0"));
            let response = request.send().await.map_err(|_| AdapterError::Network)?;
            Http::check_status(&response)?;
            connection.response = Some(response);
            connection.decoder = crate::sse::Decoder::default();
        }
        let deadline = tokio::time::Instant::now() + Duration::from_millis(800);
        loop {
            let response = connection
                .response
                .as_mut()
                .expect("event connection initialized above");
            let chunk = match tokio::time::timeout_at(deadline, response.chunk()).await {
                Err(_) => break,
                Ok(Err(_)) => {
                    *connection = crate::sse::Connection::default();
                    return Err(AdapterError::Network);
                }
                Ok(Ok(None)) => {
                    *connection = crate::sse::Connection::default();
                    break;
                }
                Ok(Ok(Some(chunk))) => chunk,
            };
            let events = match connection.decoder.push(&chunk) {
                Ok(events) => events,
                Err(error) => {
                    *connection = crate::sse::Connection::default();
                    return Err(error);
                }
            };
            for event in events {
                if event.data.is_empty() {
                    continue;
                }
                let value: Value = serde_json::from_str(&event.data)
                    .map_err(|_| AdapterError::InvalidData("invalid Signal event".into()))?;
                self.receive_changes(&value, batch);
                if let Some((key, message)) = self.receive(&value) {
                    batch.messages.push((key, vec![message]));
                }
                if event.id.is_some() {
                    batch.cursor = event.id;
                }
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Adapter for Signal {
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

    async fn login_state(&self) -> Result<crate::login::LoginState, AdapterError> {
        self.link_state().await
    }

    async fn sync(&self, cursor: Option<&str>) -> Result<SyncBatch, AdapterError> {
        let mut batch = SyncBatch {
            user_id: Some(self.active_user()?),
            cursor: cursor.map(str::to_owned),
            ..SyncBatch::default()
        };
        if cursor.is_none() {
            let contacts = self.rpc("listContacts", json!({})).await?;
            for contact in contacts.as_array().into_iter().flatten() {
                let Some(id) = self.remember_contact(contact) else {
                    continue;
                };
                let name = contact["name"]
                    .as_str()
                    .filter(|name| !name.is_empty())
                    .or(contact["profile"]["givenName"].as_str())
                    .unwrap_or(&id);
                batch
                    .conversations
                    .push(self.conversation(&id, name, RoomKind::Direct));
            }
            let groups = self.rpc("listGroups", json!({})).await?;
            for group in groups.as_array().into_iter().flatten() {
                if group["isMember"] == false {
                    continue;
                }
                let Some(id) = group["id"].as_str() else {
                    continue;
                };
                let name = group["name"].as_str().unwrap_or("Signal group");
                let mut conversation =
                    self.conversation(&format!("group:{id}"), name, RoomKind::Group);
                conversation.members = group["members"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|member| {
                        member
                            .as_str()
                            .map(str::to_owned)
                            .or_else(|| self.address(member, "number", "uuid", "recipient"))
                    })
                    .collect();
                batch.conversations.push(conversation);
            }
        }
        self.receive_batch(cursor, &mut batch).await?;
        if batch.cursor.is_none() {
            batch.cursor = Some("0".into());
        }
        Ok(batch)
    }

    async fn history(
        &self,
        room: &RoomKey,
        _before: Option<&str>,
    ) -> Result<Vec<ChatMessage>, AdapterError> {
        self.validate_room(room)?;
        Err(AdapterError::Unsupported("Signal history starts when this linked device connects. Older history is kept locally after receipt.".into()))
    }

    async fn start_conversation(&self, recipient: &str) -> Result<Conversation, AdapterError> {
        self.require(Capability::StartConversation)?;
        self.find_recipient(recipient).await
    }

    async fn send(&self, request: &SendRequest) -> Result<String, AdapterError> {
        self.validate_send(request)?;
        self.require(Capability::SendText)?;
        let mut params = Self::recipient(&request.room);
        params["message"] = json!(request.body);
        let _permit = crate::signal_media::MEDIA_JOBS
            .acquire()
            .await
            .map_err(|_| AdapterError::Network)?;
        self.add_files(request, &mut params).await?;
        if let Some(reply) = &request.reply_to {
            if let Some((author, timestamp)) = reply.rsplit_once(':') {
                let timestamp: i64 = timestamp
                    .parse()
                    .map_err(|_| AdapterError::InvalidData("invalid reply timestamp".into()))?;
                params["quoteTimestamp"] = json!(timestamp);
                params["quoteAuthor"] = json!(author);
            } else {
                return Err(AdapterError::InvalidData("invalid reply identity".into()));
            }
        }
        let result = self.rpc("send", params).await?;
        if result["results"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|entry| {
                entry["type"]
                    .as_str()
                    .is_some_and(|status| status != "SUCCESS")
            })
        {
            return Err(AdapterError::InvalidData(
                "Signal could not deliver to every recipient. Check Signal before retrying.".into(),
            ));
        }
        let timestamp = result["timestamp"]
            .as_i64()
            .ok_or_else(|| AdapterError::InvalidData("missing send timestamp".into()))?;
        Ok(format!("{}:{timestamp}", self.active_user()?))
    }

    async fn attachment(
        &self,
        room: &RoomKey,
        attachment: &messages_core::Attachment,
    ) -> Result<Vec<u8>, AdapterError> {
        self.fetch_file(room, attachment).await
    }

    async fn action(&self, room: &RoomKey, action: MessageAction) -> Result<(), AdapterError> {
        self.validate_room(room)?;
        self.require(action.capability())?;
        let mut params = Self::recipient(room);
        let method = match action {
            MessageAction::React {
                message,
                emoji,
                remove,
            } => {
                params["emoji"] = json!(emoji);
                params["remove"] = json!(remove);
                let (author, timestamp) = Self::message_identity(&message.id)?;
                params["targetAuthor"] = json!(author);
                params["targetTimestamp"] = json!(timestamp);
                "sendReaction"
            }
            MessageAction::Edit { message, body } => {
                if !message.own {
                    return Err(AdapterError::Unsupported(
                        "You can only edit your own messages.".into(),
                    ));
                }
                params["message"] = json!(body);
                params["editTimestamp"] = json!(Self::message_identity(&message.id)?.1);
                "send"
            }
            MessageAction::Delete { message } => {
                if !message.own {
                    return Err(AdapterError::Unsupported(
                        "You can only delete your own messages.".into(),
                    ));
                }
                params["targetTimestamp"] = json!(Self::message_identity(&message.id)?.1);
                "remoteDelete"
            }
            MessageAction::Read { message } => {
                let (author, timestamp) = Self::message_identity(&message)?;
                params = json!({"recipient":author});
                params["targetTimestamp"] = json!([timestamp]);
                params["type"] = json!("read");
                "sendReceipt"
            }
            MessageAction::Typing { active } => {
                params["stop"] = json!(!active);
                "sendTyping"
            }
        };
        self.rpc(method, params).await?;
        Ok(())
    }
}
