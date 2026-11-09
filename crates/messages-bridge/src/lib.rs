//! Local native protocol hosts. Cryptographic sessions stay in the owning SDK;
//! the desktop receives a versioned, capability-limited view of the account.

pub mod cache;
pub mod host;
#[cfg(feature = "matrix")]
pub mod matrix;
#[cfg(feature = "whatsapp")]
pub mod whatsapp;

use async_trait::async_trait;
use messages_protocols::bridge::BridgeStatus;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Deserialize, Serialize)]
pub struct Command {
    pub chat_id: String,
    #[serde(default)]
    pub attachments: Vec<messages_protocols::media::WireAttachment>,
    #[serde(default)]
    pub attachment_id: String,
    #[serde(default)]
    pub message_id: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub sender: String,
    #[serde(default)]
    pub own: bool,
    #[serde(default)]
    pub emoji: String,
    #[serde(default)]
    pub remove: bool,
    #[serde(default)]
    pub active: bool,
    pub transaction_id: Option<uuid::Uuid>,
    pub reply_to: Option<String>,
    pub before: Option<String>,
}

#[async_trait]
pub trait Backend: Send + Sync {
    async fn avatar(&self, _chat: &str) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(None)
    }

    async fn browser_start(&self, _request: uuid::Uuid) -> anyhow::Result<Value> {
        anyhow::bail!("Browser sign-in is unavailable")
    }
    async fn browser_cancel(&self, _request: uuid::Uuid) -> anyhow::Result<()> {
        Ok(())
    }
    async fn call_control(
        &self,
        _request: messages_protocols::calls::CallRequest,
    ) -> anyhow::Result<messages_protocols::calls::CallSnapshot> {
        anyhow::bail!("Calling is not available on this connection")
    }
    async fn calls(&self) -> Vec<messages_protocols::calls::CallInvite> {
        Vec::new()
    }
    async fn status(&self) -> BridgeStatus;
    async fn command(&self, operation: &str, command: Command) -> anyhow::Result<Value>;
    async fn login(&self) -> Value;
    async fn authenticate(&self, input: Value) -> anyhow::Result<()>;
    fn cache(&self) -> &cache::SharedCache;
}
