//! Versioned local bridge contract for stateful native protocol libraries.
//! Native Rust SDKs own WhatsApp and Matrix sessions. Their cryptographic
//! state stays in the bridge while the desktop speaks a bounded JSON API.

use async_trait::async_trait;
use messages_core::{Capabilities, Capability, ChatMessage, Conversation, RoomKey, SendRequest};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    Adapter, AdapterError, ConnectionConfig, MessageAction, SyncBatch, http::Http, matrix::string,
};

pub const API_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BridgeStatus {
    pub api_version: u32,
    pub protocol: String,
    pub authenticated: bool,
    pub user_id: Option<String>,
    pub capabilities: Capabilities,
    pub login_state: Option<String>,
}

pub struct Bridge {
    config: ConnectionConfig,
    http: Http,
    capabilities: std::sync::RwLock<Capabilities>,
}

impl Bridge {
    pub fn new(config: ConnectionConfig, token: Option<String>) -> Result<Self, AdapterError> {
        let http = Http::new(
            &config.endpoint,
            token.map(|value| format!("Bearer {value}")),
        )?;
        Ok(Self {
            config,
            http,
            capabilities: std::sync::RwLock::new(Capabilities::default()),
        })
    }

    fn local_media(&self) -> bool {
        url::Url::parse(&self.config.endpoint)
            .ok()
            .and_then(|u| {
                u.host().map(|h| match h {
                    url::Host::Domain(name) => name == "localhost",
                    url::Host::Ipv4(ip) => ip.is_loopback(),
                    url::Host::Ipv6(ip) => ip.is_loopback(),
                })
            })
            .unwrap_or(false)
    }

    pub async fn status(&self) -> Result<BridgeStatus, AdapterError> {
        let value = self
            .http
            .json(Method::GET, &["v1", "status"], &[], None)
            .await?;
        let mut status: BridgeStatus = serde_json::from_value(value)
            .map_err(|_| AdapterError::InvalidData("invalid bridge status".into()))?;
        if status.api_version != API_VERSION || status.protocol != self.config.protocol.as_str() {
            return Err(AdapterError::Configuration(
                "The bridge version or protocol does not match this account.".into(),
            ));
        }
        if self.config.protocol.as_str() != "whatsapp" || !self.local_media() {
            for capability in [Capability::VoiceCall, Capability::VideoCall] {
                status
                    .capabilities
                    .disable(capability, "Calling requires the local WhatsApp helper.");
            }
        }
        *self
            .capabilities
            .write()
            .map_err(|_| AdapterError::InvalidData("bridge state unavailable".into()))? =
            status.capabilities.clone();
        Ok(status)
    }

    async fn call(&self, operation: &str, payload: Value) -> Result<Value, AdapterError> {
        self.http
            .json(Method::POST, &["v1", operation], &[], Some(payload))
            .await
    }
}

#[async_trait]
impl Adapter for Bridge {
    fn supports_encryption_recovery(&self) -> bool {
        self.config.protocol.as_str() == "matrix" && self.local_media()
    }
    async fn call_control(
        &self,
        room: &RoomKey,
        command: crate::calls::CallCommand,
    ) -> Result<crate::calls::CallSnapshot, AdapterError> {
        if room.account != self.config.account {
            return Err(AdapterError::Configuration(
                "The call belongs to another account.".into(),
            ));
        }
        let capability = match &command {
            crate::calls::CallCommand::Start { kind, .. } => kind.capability(),
            crate::calls::CallCommand::Camera { .. } => Capability::VideoCall,
            _ => Capability::VoiceCall,
        };
        if !matches!(command, crate::calls::CallCommand::End { .. }) {
            self.capabilities()
                .require(capability)
                .map_err(AdapterError::Unsupported)?;
        }
        let value = self
            .http
            .json(
                Method::POST,
                &["v1", "call"],
                &[],
                Some(
                    serde_json::to_value(crate::calls::CallRequest {
                        chat_id: room.room.clone(),
                        command,
                    })
                    .map_err(|_| AdapterError::InvalidData("Invalid call request".into()))?,
                ),
            )
            .await?;
        serde_json::from_value(value)
            .map_err(|_| AdapterError::InvalidData("Invalid call response".into()))
    }

    fn supports_avatars(&self) -> bool {
        self.config.protocol.as_str() == "whatsapp"
    }

    async fn avatar(&self, room: &RoomKey) -> Result<Option<Vec<u8>>, AdapterError> {
        self.validate_room(room)?;
        if !self.supports_avatars() {
            return Ok(None);
        }
        let mut response = self
            .http
            .request(Method::GET, self.http.url(&["v1", "avatar"])?)
            .query(&[("chat_id", &room.room)])
            .send()
            .await
            .map_err(|_| AdapterError::Network)?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        crate::http::Http::check_status(&response)?;
        let mut bytes = Vec::new();
        if response.content_length().is_some_and(|n| n > 262144) {
            return Err(AdapterError::InvalidData(
                "Profile photo exceeds size limit".into(),
            ));
        }
        while let Some(chunk) = response.chunk().await.map_err(|_| AdapterError::Network)? {
            if bytes.len() + chunk.len() > 262144 {
                return Err(AdapterError::InvalidData(
                    "Profile photo exceeds size limit".into(),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(Some(bytes))
    }

    fn config(&self) -> &ConnectionConfig {
        &self.config
    }
    fn capabilities(&self) -> Capabilities {
        self.capabilities
            .read()
            .map(|caps| caps.clone())
            .unwrap_or_default()
    }

    async fn begin_browser_login(
        &self,
        request: uuid::Uuid,
    ) -> Result<crate::login::BrowserChallenge, AdapterError> {
        if self.config.protocol.as_str() != "matrix" || !self.local_media() {
            return Err(AdapterError::Unsupported(
                "Browser sign-in requires the local Matrix helper.".into(),
            ));
        }
        let response = self
            .http
            .request(Method::POST, self.http.url(&["v1", "browser"])?)
            .json(&json!({"request_id":request}))
            .send()
            .await
            .map_err(|_| AdapterError::Network)?;
        if response.status() == reqwest::StatusCode::BAD_REQUEST {
            // OAuth response bodies can contain credentials; admit only typed helper codes.
            let value = Http::payload_limited(response, 16384).await?;
            let failure = value
                .get("error_code")
                .cloned()
                .and_then(|code| serde_json::from_value::<crate::login::BrowserFailure>(code).ok())
                .unwrap_or(crate::login::BrowserFailure::StartFailed);
            return Err(AdapterError::Configuration(failure.to_string()));
        }
        let value = Http::decode(response).await?;
        crate::login::validate_browser_url(&string(&value, "url")?)?;
        if value["request_id"].as_str() != Some(request.to_string().as_str()) {
            return Err(AdapterError::InvalidData(
                "Wrong browser sign-in request".into(),
            ));
        }
        Ok(crate::login::BrowserChallenge {
            url: string(&value, "url")?.into(),
            expires_in: value["expires_in"].as_u64().unwrap_or(600).min(600),
        })
    }
    async fn cancel_browser_login(&self, request: uuid::Uuid) -> Result<(), AdapterError> {
        self.http
            .json(
                Method::DELETE,
                &["v1", "browser"],
                &[],
                Some(json!({"request_id":request})),
            )
            .await?;
        Ok(())
    }

    async fn login_state(&self) -> Result<crate::login::LoginState, AdapterError> {
        use crate::login::LoginState;
        self.status().await?;
        let value = self
            .http
            .json(Method::GET, &["v1", "login"], &[], None)
            .await?;
        match value["state"].as_str() {
            Some("qr") => Ok(LoginState::Qr {
                payload: string(&value, "qr")?.into(),
                expires_in: value["expires_in"].as_u64().unwrap_or(30).min(300),
            }),
            Some("phone") => Ok(LoginState::Phone),
            Some("code") => Ok(LoginState::Code),
            Some("password") => Ok(LoginState::Password),
            Some("matrix_recovery")
                if self.config.protocol.as_str() == "matrix" && self.local_media() =>
            {
                Ok(LoginState::MatrixRecovery)
            }
            Some("matrix") => Ok(LoginState::Matrix {
                password: value["password"].as_bool().unwrap_or(false),
                browser: value["browser"].as_bool().unwrap_or(false) && self.local_media(),
                problem: value["problem"].as_str().map(str::to_owned),
            }),
            Some("browser") => Ok(LoginState::Browser {
                request_id: uuid::Uuid::parse_str(&string(&value, "request_id")?).map_err(
                    |_| AdapterError::InvalidData("Invalid browser sign-in request".into()),
                )?,
                url: string(&value, "url")?.into(),
                expires_in: value["expires_in"].as_u64().unwrap_or(600).min(600),
            }),
            Some("ready") => Ok(LoginState::Ready),
            Some("unsupported") => Ok(LoginState::Unsupported {
                reason: string(&value, "reason")?,
            }),
            Some("connecting") => Ok(LoginState::Connecting),
            _ => Err(AdapterError::InvalidData(
                "unknown bridge authentication state".into(),
            )),
        }
    }

    async fn authenticate(&self, input: crate::login::LoginInput) -> Result<(), AdapterError> {
        if input.recover && !self.supports_encryption_recovery() {
            return Err(AdapterError::Unsupported(
                "Encryption recovery requires the local Matrix helper.".into(),
            ));
        }
        self.status().await?;
        self.call(
            "login",
            json!({"user":input.user,"value":input.value.expose(),
            "operation":if input.recover { "recover" } else { "login" }}),
        )
        .await.map_err(|error|if input.recover && matches!(error, AdapterError::Http(400)) { AdapterError::Configuration("Could not restore encryption keys. Check your recovery key or passphrase and connection, then try again.".into()) } else { error })?;
        Ok(())
    }

    async fn sync(&self, cursor: Option<&str>) -> Result<SyncBatch, AdapterError> {
        let status = self.status().await?;
        if !status.authenticated {
            return Err(
                if matches!(
                    self.login_state().await?,
                    crate::login::LoginState::Connecting
                ) {
                    AdapterError::Network
                } else {
                    AdapterError::Authentication
                },
            );
        }
        let query: Vec<_> = cursor.map(|cursor| ("since", cursor)).into_iter().collect();
        let mut data = self
            .http
            .json(Method::GET, &["v1", "chats"], &query, None)
            .await?;
        let mut conversations: Vec<Conversation> = serde_json::from_value(
            data.get_mut("conversations")
                .map(Value::take)
                .unwrap_or(Value::Null),
        )
        .map_err(|_| AdapterError::InvalidData("invalid bridge conversations".into()))?;
        let capabilities = self.capabilities();
        for conversation in &mut conversations {
            conversation.key.account = self.config.account;
            conversation.capabilities = capabilities.restricted_by(&conversation.capabilities);
        }
        let mut read_through: Vec<(RoomKey, i64)> =
            serde_json::from_value(data["read_through"].clone()).unwrap_or_default();
        for (key, _) in &mut read_through {
            key.account = self.config.account;
        }
        let mut typing: Vec<crate::TypingUpdate> =
            serde_json::from_value(data["typing"].clone()).unwrap_or_default();
        for typing in &mut typing {
            typing.room.account = self.config.account;
        }
        let mut calls: Vec<crate::calls::CallInvite> =
            serde_json::from_value(data["calls"].clone()).unwrap_or_default();
        calls.truncate(1);
        for invite in &mut calls {
            invite.room.account = self.config.account;
        }
        Ok(SyncBatch {
            calls,
            read_through,
            typing,
            user_id: status.user_id,
            conversations,
            cursor: data["cursor"].as_str().map(str::to_owned),
            ..SyncBatch::default()
        })
    }

    async fn start_thread(
        &self,
        parent: &RoomKey,
        name: &str,
    ) -> Result<Conversation, AdapterError> {
        self.validate_room(parent)?;
        self.require(Capability::Threads)?;
        let data = self
            .call("thread", json!({"chat_id":parent.room,"body":name}))
            .await?;
        let mut room: Conversation = serde_json::from_value(data["conversation"].clone())
            .map_err(|_| AdapterError::InvalidData("Invalid thread".into()))?;
        room.key.account = parent.account;
        if room.parent_room.as_ref() != Some(&parent.room) {
            return Err(AdapterError::InvalidData(
                "Thread belongs to another conversation".into(),
            ));
        }
        Ok(room)
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
        self.validate_room(room)?;
        let data = self
            .call(
                "history",
                json!({"chat_id":room.room,"before":before,"limit":50}),
            )
            .await?;
        let messages: Vec<ChatMessage> = serde_json::from_value(data["messages"].clone())
            .map_err(|_| AdapterError::InvalidData("invalid bridge history".into()))?;
        let next = if data.get("next").is_some() {
            data["next"].as_str().map(str::to_owned)
        } else {
            (messages.len() >= 50)
                .then(|| messages.first().map(|message| message.id.clone()))
                .flatten()
        };
        Ok(crate::HistoryPage { messages, next })
    }

    async fn start_conversation(&self, recipient: &str) -> Result<Conversation, AdapterError> {
        let status = self.status().await?;
        if !status.authenticated {
            return Err(AdapterError::Authentication);
        }
        self.require(Capability::StartConversation)?;
        if recipient.trim().is_empty() || recipient.len() > 256 {
            return Err(AdapterError::Configuration(
                "Enter a valid recipient or conversation ID.".into(),
            ));
        }
        let data = self
            .call("open", json!({"chat_id":recipient.trim()}))
            .await?;
        let mut conversation: Conversation =
            serde_json::from_value(data["conversation"].clone())
                .map_err(|_| AdapterError::InvalidData("invalid new conversation".into()))?;
        conversation.key.account = self.config.account;
        self.validate_room(&conversation.key)?;
        conversation.capabilities = self
            .capabilities()
            .restricted_by(&conversation.capabilities);
        Ok(conversation)
    }

    async fn send(&self, request: &SendRequest) -> Result<String, AdapterError> {
        self.validate_send(request)?;
        self.require(Capability::SendText)?;
        if request.reply_to.is_some() {
            self.require(Capability::Reply)?;
        }
        let value = self
            .call(
                "send",
                json!({"chat_id":request.room.room,"body":request.body,
            "transaction_id":request.transaction,"reply_to":request.reply_to, "attachments":crate::media::encode(&request.attachments).await?}),
            )
            .await?;
        string(&value, "message_id")
    }

    async fn attachment(
        &self,
        room: &RoomKey,
        attachment: &messages_core::Attachment,
    ) -> Result<Vec<u8>, AdapterError> {
        self.validate_room(room)?;
        if attachment.bytes > messages_core::MAX_ATTACHMENT_BYTES {
            return Err(AdapterError::Configuration(
                "File exceeds the download limit".into(),
            ));
        }
        let response = self
            .http
            .request(Method::POST, self.http.url(&["v1", "attachment"])?)
            .json(&json!({"chat_id":room.room,"attachment_id":attachment.id}))
            .send()
            .await
            .map_err(|_| AdapterError::Network)?;
        let value = Http::decode_limited(response, 140 * 1024 * 1024).await?;
        let file: crate::media::WireAttachment =
            serde_json::from_value(value["attachment"].clone())
                .map_err(|_| AdapterError::InvalidData("Invalid bridge file response".into()))?;
        tokio::task::spawn_blocking(move || crate::media::decode(&file))
            .await
            .map_err(|_| AdapterError::Network)?
    }

    async fn action(&self, room: &RoomKey, action: MessageAction) -> Result<(), AdapterError> {
        self.validate_room(room)?;
        self.require(action.capability())?;
        let (operation, mut payload) = match action {
            MessageAction::React {
                message,
                emoji,
                remove,
            } => (
                "react",
                json!({"message_id":message.id,
                "sender":message.sender,"own":message.own,"emoji":emoji,"remove":remove}),
            ),
            MessageAction::Edit { message, body } => {
                if !message.own {
                    return Err(AdapterError::Unsupported(
                        "You can only edit your own messages.".into(),
                    ));
                }
                ("edit", json!({"message_id":message.id,"body":body}))
            }
            MessageAction::Delete { message } => {
                if !message.own {
                    return Err(AdapterError::Unsupported(
                        "You can only delete your own messages.".into(),
                    ));
                }
                ("delete", json!({"message_id":message.id}))
            }
            MessageAction::Read { message } => ("read", json!({"message_id":message})),
            MessageAction::Typing { active } => ("typing", json!({"active":active})),
        };
        payload["chat_id"] = json!(room.room);
        self.call(operation, payload).await?;
        Ok(())
    }
}
