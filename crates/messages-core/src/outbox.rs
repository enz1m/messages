use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::RoomKey;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SendRequest {
    pub transaction: Uuid,
    pub room: RoomKey,
    pub body: String,
    pub reply_to: Option<String>,
    #[serde(default)]
    pub attachments: Vec<OutgoingAttachment>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "detail", rename_all = "snake_case")]
pub enum OutboxState {
    Queued,
    Sending,
    Failed(String),
    /// The request may have reached a non-idempotent remote API.
    Uncertain,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OutboxItem {
    pub request: SendRequest,
    pub created_at: DateTime<Utc>,
    pub attempts: u32,
    pub state: OutboxState,
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum SendError {
    #[error("{0}")]
    Rejected(String),
    #[error("The connection ended before delivery could be confirmed.")]
    Uncertain,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OutgoingAttachment {
    pub id: Uuid,
    pub path: std::path::PathBuf,
    pub name: String,
    pub mime: String,
    pub bytes: u64,
}

pub const MAX_ATTACHMENT_BYTES: u64 = 100 * 1024 * 1024;
pub const MAX_ATTACHMENTS: usize = 10;
