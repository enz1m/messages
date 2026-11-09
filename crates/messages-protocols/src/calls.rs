use messages_core::{Capability, RoomKey};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallKind {
    Voice,
    Video,
}
impl CallKind {
    pub fn capability(self) -> Capability {
        match self {
            Self::Voice => Capability::VoiceCall,
            Self::Video => Capability::VideoCall,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallPhase {
    Incoming,
    Calling,
    Connecting,
    Connected,
    Ended,
    Failed,
}
impl CallPhase {
    pub fn finished(self) -> bool {
        matches!(self, Self::Ended | Self::Failed)
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Incoming => "Incoming call",
            Self::Calling => "Calling…",
            Self::Connecting => "Connecting…",
            Self::Connected => "Connected",
            Self::Ended => "Call ended",
            Self::Failed => "Call could not be completed",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum CallCommand {
    Start {
        id: Uuid,
        kind: CallKind,
        devices: messages_media::Selection,
        expires_at: i64,
    },
    Accept {
        id: Uuid,
        devices: messages_media::Selection,
    },
    Poll {
        id: Uuid,
    },
    Camera {
        id: Uuid,
        enabled: bool,
    },
    Mute {
        id: Uuid,
        muted: bool,
    },
    End {
        id: Uuid,
    },
}
impl CallCommand {
    pub fn id(&self) -> Uuid {
        match self {
            Self::Start { id, .. }
            | Self::Accept { id, .. }
            | Self::Poll { id }
            | Self::Camera { id, .. }
            | Self::Mute { id, .. }
            | Self::End { id } => *id,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CallRequest {
    pub chat_id: String,
    pub command: CallCommand,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CallSnapshot {
    pub id: Uuid,
    pub kind: CallKind,
    #[serde(default = "unavailable_encryption")]
    pub encryption: messages_core::Encryption,
    pub phase: CallPhase,
    pub muted: bool,
    #[serde(default)]
    pub camera_on: bool,
    #[serde(default)]
    pub preview_png: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CallInvite {
    pub room: RoomKey,
    pub call: CallSnapshot,
}

fn unavailable_encryption() -> messages_core::Encryption {
    messages_core::Encryption::Unavailable
}
