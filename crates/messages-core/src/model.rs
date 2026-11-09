use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Capabilities;

/// Stable adapter registry key. Third-party adapters need no new core enum arm.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProtocolId(pub String);

impl ProtocolId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ProtocolId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

pub type AccountId = Uuid;

/// The remote ID alone is not unique across accounts or protocols.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct RoomKey {
    pub account: AccountId,
    pub room: String,
}

impl RoomKey {
    pub fn new(account: AccountId, room: impl Into<String>) -> Self {
        Self {
            account,
            room: room.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "detail", rename_all = "snake_case")]
pub enum ConnectionState {
    Demo,
    Disconnected,
    Connecting,
    Online,
    NeedsAuthentication(String),
    Failed(String),
}

impl ConnectionState {
    pub fn label(&self) -> &str {
        match self {
            Self::Demo => "Sample account",
            Self::Disconnected => "Offline",
            Self::Connecting => "Connecting…",
            Self::Online => "Connected",
            Self::NeedsAuthentication(_) => "Sign-in required",
            Self::Failed(_) => "Connection issue",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Account {
    pub id: AccountId,
    pub protocol: ProtocolId,
    pub label: String,
    pub user_id: String,
    pub connection: ConnectionState,
    pub capabilities: Capabilities,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomKind {
    Direct,
    Group,
    Channel,
}

/// A transport being TLS-protected does not mean the conversation is end-to-end encrypted.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Encryption {
    EndToEnd,
    Transport,
    Unavailable,
    Sample,
}

impl Encryption {
    pub fn label(self) -> &'static str {
        match self {
            Self::EndToEnd => "End-to-end encrypted",
            Self::Transport => "Transport encrypted",
            Self::Unavailable => "Encryption unavailable",
            Self::Sample => "Sample conversation",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Conversation {
    pub key: RoomKey,
    pub title: String,
    pub subtitle: String,
    #[serde(default)]
    pub avatar_revision: u64,
    /// Optional server/group label and owning channel for thread navigation.
    #[serde(default)]
    pub group_name: Option<String>,
    #[serde(default)]
    pub parent_room: Option<String>,
    pub kind: RoomKind,
    pub members: Vec<String>,
    pub unread: u32,
    pub pinned: bool,
    pub muted: bool,
    #[serde(default)]
    pub muted_until: Option<DateTime<Utc>>,
    pub archived: bool,
    pub encryption: Encryption,
    pub capabilities: Capabilities,
    pub messages: Vec<ChatMessage>,
    pub draft: String,
    #[serde(default)]
    pub draft_attachments: Vec<crate::OutgoingAttachment>,
    pub reply_to: Option<String>,
    pub has_older: bool,
    pub pagination: Option<String>,
}

impl Conversation {
    pub fn empty(
        key: RoomKey,
        title: impl Into<String>,
        kind: RoomKind,
        encryption: Encryption,
        capabilities: Capabilities,
    ) -> Self {
        Self {
            key,
            title: title.into(),
            subtitle: String::new(),
            avatar_revision: 0,
            kind,
            members: Vec::new(),
            unread: 0,
            pinned: false,
            muted: false,
            muted_until: None,
            group_name: None,
            parent_room: None,
            archived: false,
            encryption,
            capabilities,
            messages: Vec::new(),
            draft: String::new(),
            draft_attachments: Vec::new(),
            reply_to: None,
            has_older: false,
            pagination: None,
        }
    }

    pub fn preview(&self) -> &str {
        self.readable_messages()
            .next_back()
            .map(|m| {
                if m.deleted {
                    "Message deleted"
                } else if m.body.trim().is_empty() {
                    m.attachments
                        .first()
                        .map(|file| file.name.as_str())
                        .unwrap_or(&m.body)
                } else {
                    &m.body
                }
            })
            .unwrap_or_else(|| {
                if self.has_locked_history() {
                    "Encrypted history · Restore keys"
                } else {
                    "No messages yet"
                }
            })
    }

    pub fn readable_messages(&self) -> impl DoubleEndedIterator<Item = &ChatMessage> {
        self.messages.iter().filter(|message| message.is_readable())
    }

    pub fn has_locked_history(&self) -> bool {
        self.messages.iter().any(ChatMessage::waiting_for_keys)
    }

    pub fn updated_at(&self) -> Option<DateTime<Utc>> {
        self.messages.last().map(|m| m.timestamp)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "detail", rename_all = "snake_case")]
pub enum Delivery {
    Received,
    Queued,
    Sending,
    Sent,
    Delivered,
    Read,
    Failed(String),
    Uncertain,
}

impl Delivery {
    pub fn label(&self) -> &str {
        match self {
            Self::Received => "",
            Self::Queued => "Queued",
            Self::Sending => "Sending…",
            Self::Sent => "Sent",
            Self::Delivered => "Delivered",
            Self::Read => "Read",
            Self::Failed(_) => "Not sent",
            Self::Uncertain => "Delivery unconfirmed",
        }
    }

    pub(crate) fn receipt_rank(&self) -> u8 {
        match self {
            Self::Sent => 1,
            Self::Delivered => 2,
            Self::Read => 3,
            _ => 0,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Reaction {
    pub emoji: String,
    pub count: u32,
    pub own: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Attachment {
    pub id: String,
    pub name: String,
    pub mime: String,
    pub bytes: u64,
    pub remote_url: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageContentState {
    #[default]
    Available,
    WaitingForKeys,
    /// A decrypted relation retires its earlier placeholder across cached snapshots.
    Relation,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: String,
    pub transaction: Option<Uuid>,
    pub sender: String,
    pub sender_name: String,
    pub own: bool,
    pub body: String,
    /// Absent in legacy snapshots; adapters publish an explicit state for new messages.
    #[serde(default)]
    pub content_state: Option<MessageContentState>,
    pub timestamp: DateTime<Utc>,
    pub delivery: Delivery,
    pub edited: bool,
    pub deleted: bool,
    pub reply_to: Option<String>,
    pub reactions: Vec<Reaction>,
    pub attachments: Vec<Attachment>,
}

impl ChatMessage {
    pub fn is_readable(&self) -> bool {
        !self.waiting_for_keys() && self.content_state != Some(MessageContentState::Relation)
    }
    pub fn waiting_for_keys(&self) -> bool {
        !self.deleted && self.content_state == Some(MessageContentState::WaitingForKeys)
    }
}
