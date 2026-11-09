use async_trait::async_trait;
use messages_core::{
    AccountId, Capabilities, Capability, ChatMessage, Conversation, ProtocolId, RoomKey,
    SendRequest,
};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Only non-secret connection settings are serializable. Tokens are supplied
/// separately when constructing an adapter.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConnectionConfig {
    pub account: AccountId,
    pub protocol: ProtocolId,
    pub label: String,
    pub endpoint: String,
    pub user_id: String,
    #[serde(default)]
    pub rooms: Vec<String>,
    #[serde(default)]
    pub bot: bool,
    #[serde(default)]
    pub native_bridge: bool,
    #[serde(default)]
    pub managed: Option<ManagedConnection>,
}

#[derive(Clone, Debug)]
pub enum ManagedConnection {
    Signal,
    WhatsApp,
    Matrix { homeserver: String },
    Retired { service: String },
}

impl Serialize for ManagedConnection {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let value = match self {
            Self::Signal => serde_json::json!({"service":"signal"}),
            Self::WhatsApp => serde_json::json!({"service":"whatsapp"}),
            Self::Matrix { homeserver } => {
                serde_json::json!({"service":"matrix","homeserver":homeserver})
            }
            Self::Retired { service } => serde_json::json!({"service":service}),
        };
        value.serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for ManagedConnection {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        let service = value["service"]
            .as_str()
            .ok_or_else(|| serde::de::Error::custom("missing managed service"))?;
        Ok(match service {
            "signal" => Self::Signal,
            "whatsapp" => Self::WhatsApp,
            "matrix" => Self::Matrix {
                homeserver: value["homeserver"]
                    .as_str()
                    .ok_or_else(|| serde::de::Error::custom("missing homeserver"))?
                    .into(),
            },
            _ => Self::Retired {
                service: service.into(),
            },
        })
    }
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("{0}")]
    Unsupported(String),
    #[error("Sign in again to reconnect this account.")]
    Authentication,
    #[error("The server asked us to wait before trying again.")]
    RateLimited(Duration),
    #[error("The server could not complete this request (HTTP {0}).")]
    Http(u16),
    #[error("Could not reach the server. Check the connection and try again.")]
    Network,
    #[error("The server returned an unexpected response: {0}")]
    InvalidData(String),
    #[error("{0}")]
    Configuration(String),
}

impl AdapterError {
    /// Network/5xx failures can occur after the server accepted a write.
    pub fn delivery_is_uncertain(&self) -> bool {
        matches!(
            self,
            Self::Network | Self::Http(500..=599) | Self::InvalidData(_)
        )
    }
}

#[derive(Clone, Debug, Default)]
pub struct SyncBatch {
    pub calls: Vec<crate::calls::CallInvite>,
    pub user_id: Option<String>,
    pub conversations: Vec<Conversation>,
    pub messages: Vec<(RoomKey, Vec<ChatMessage>)>,
    pub cursor: Option<String>,
    pub changes: Vec<messages_core::MessageChange>,
    pub typing: Vec<TypingUpdate>,
    pub device_reads: Vec<messages_core::DeviceRead>,
    pub read_through: Vec<(RoomKey, i64)>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TypingUpdate {
    pub room: RoomKey,
    pub actor: String,
    pub name: String,
    pub active: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HistoryPage {
    pub messages: Vec<ChatMessage>,
    pub next: Option<String>,
}

#[derive(Clone, Debug)]
pub enum MessageAction {
    React {
        message: ChatMessage,
        emoji: String,
        remove: bool,
    },
    Edit {
        message: ChatMessage,
        body: String,
    },
    Delete {
        message: ChatMessage,
    },
    Read {
        message: String,
    },
    Typing {
        active: bool,
    },
}

impl MessageAction {
    pub fn capability(&self) -> Capability {
        match self {
            Self::React { .. } => Capability::React,
            Self::Edit { .. } => Capability::EditOwn,
            Self::Delete { .. } => Capability::DeleteOwn,
            Self::Read { .. } => Capability::ReadReceipts,
            Self::Typing { .. } => Capability::Typing,
        }
    }
}

#[async_trait]
pub trait Adapter: Send + Sync {
    fn supports_encryption_recovery(&self) -> bool {
        false
    }
    fn supports_avatars(&self) -> bool {
        false
    }
    async fn avatar(&self, _room: &RoomKey) -> Result<Option<Vec<u8>>, AdapterError> {
        Ok(None)
    }

    async fn begin_browser_login(
        &self,
        _request: uuid::Uuid,
    ) -> Result<crate::login::BrowserChallenge, AdapterError> {
        Err(AdapterError::Unsupported(
            "Browser sign-in is not available on this connection.".into(),
        ))
    }
    async fn cancel_browser_login(&self, _request: uuid::Uuid) -> Result<(), AdapterError> {
        Ok(())
    }
    fn config(&self) -> &ConnectionConfig;
    fn capabilities(&self) -> Capabilities;
    async fn call_control(
        &self,
        _room: &RoomKey,
        _command: crate::calls::CallCommand,
    ) -> Result<crate::calls::CallSnapshot, AdapterError> {
        Err(AdapterError::Unsupported(
            "Calling is not available on this connection.".into(),
        ))
    }
    fn idempotent_sends(&self) -> bool {
        false
    }
    async fn sync(&self, cursor: Option<&str>) -> Result<SyncBatch, AdapterError>;
    async fn history(
        &self,
        room: &RoomKey,
        before: Option<&str>,
    ) -> Result<Vec<ChatMessage>, AdapterError>;
    async fn history_page(
        &self,
        room: &RoomKey,
        before: Option<&str>,
    ) -> Result<HistoryPage, AdapterError> {
        let messages = self.history(room, before).await?;
        let next = (messages.len() >= 50)
            .then(|| messages.first().map(|message| message.id.clone()))
            .flatten();
        Ok(HistoryPage { messages, next })
    }
    async fn attachment(
        &self,
        _room: &RoomKey,
        _attachment: &messages_core::Attachment,
    ) -> Result<Vec<u8>, AdapterError> {
        Err(AdapterError::Unsupported(
            "Downloading files is not available on this connection.".into(),
        ))
    }

    fn validate_send(&self, request: &SendRequest) -> Result<(), AdapterError> {
        self.validate_room(&request.room)?;
        self.require(Capability::SendText)?;
        if !request.attachments.is_empty() {
            self.require(Capability::AttachFiles)?;
        }
        Ok(())
    }

    async fn send(&self, request: &SendRequest) -> Result<String, AdapterError>;
    async fn action(&self, room: &RoomKey, action: MessageAction) -> Result<(), AdapterError>;

    async fn start_conversation(&self, _recipient: &str) -> Result<Conversation, AdapterError> {
        Err(AdapterError::Unsupported(
            "Add a conversation through this platform's account settings.".into(),
        ))
    }

    async fn start_thread(
        &self,
        _parent: &RoomKey,
        _name: &str,
    ) -> Result<Conversation, AdapterError> {
        Err(AdapterError::Unsupported(
            "This conversation does not support threads.".into(),
        ))
    }

    async fn login_state(&self) -> Result<crate::login::LoginState, AdapterError> {
        Err(AdapterError::Unsupported(
            "Reconnect with an access token in Settings.".into(),
        ))
    }

    async fn authenticate(&self, _input: crate::login::LoginInput) -> Result<(), AdapterError> {
        Err(AdapterError::Unsupported(
            "This connection uses an access token.".into(),
        ))
    }

    fn validate_room(&self, room: &RoomKey) -> Result<(), AdapterError> {
        if room.account != self.config().account {
            Err(AdapterError::Configuration(
                "This conversation belongs to another account.".into(),
            ))
        } else if room.room.is_empty() {
            Err(AdapterError::Configuration(
                "The conversation ID is empty.".into(),
            ))
        } else {
            Ok(())
        }
    }

    fn require(&self, capability: Capability) -> Result<(), AdapterError> {
        self.capabilities()
            .require(capability)
            .map_err(AdapterError::Unsupported)
    }
}
