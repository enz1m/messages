use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Actions are negotiated per account and conversation, never inferred from an icon.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    StartConversation,
    SendText,
    Reply,
    React,
    EditOwn,
    DeleteOwn,
    AttachFiles,
    ReadReceipts,
    Typing,
    Threads,
    VoiceCall,
    VideoCall,
}

impl Capability {
    pub const ALL: [Self; 12] = [
        Self::StartConversation,
        Self::SendText,
        Self::Reply,
        Self::React,
        Self::EditOwn,
        Self::DeleteOwn,
        Self::AttachFiles,
        Self::ReadReceipts,
        Self::Typing,
        Self::Threads,
        Self::VoiceCall,
        Self::VideoCall,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::StartConversation => "New conversations",
            Self::SendText => "Messages",
            Self::Reply => "Replies",
            Self::React => "Reactions",
            Self::EditOwn => "Edit messages",
            Self::DeleteOwn => "Delete messages",
            Self::AttachFiles => "Attachments",
            Self::ReadReceipts => "Read receipts",
            Self::Typing => "Typing indicators",
            Self::Threads => "Threads",
            Self::VoiceCall => "Voice calls",
            Self::VideoCall => "Video calls",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "reason", rename_all = "snake_case")]
pub enum Availability {
    Available,
    Unavailable(String),
}

impl Availability {
    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Available => None,
            Self::Unavailable(reason) => Some(reason),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Capabilities(BTreeMap<Capability, Availability>);

impl Capabilities {
    pub fn new(enabled: impl IntoIterator<Item = Capability>) -> Self {
        Self(
            enabled
                .into_iter()
                .map(|key| (key, Availability::Available))
                .collect(),
        )
    }

    pub fn get(&self, capability: Capability) -> Availability {
        self.0.get(&capability).cloned().unwrap_or_else(|| {
            Availability::Unavailable("This connection does not support this action yet.".into())
        })
    }

    pub fn supports(&self, capability: Capability) -> bool {
        self.get(capability).is_available()
    }

    /// Declare an action negotiated by the adapter as available.
    pub fn enable(&mut self, capability: Capability) {
        self.0.insert(capability, Availability::Available);
    }

    pub fn disable(&mut self, capability: Capability, reason: impl Into<String>) {
        self.0
            .insert(capability, Availability::Unavailable(reason.into()));
    }

    /// A room can restrict an account's abilities, but cannot grant new ones.
    pub fn restricted_by(&self, room: &Self) -> Self {
        let mut result = self.clone();
        for capability in Capability::ALL {
            if !room.supports(capability) {
                result.0.insert(capability, room.get(capability));
            }
        }
        result
    }

    pub fn require(&self, capability: Capability) -> Result<(), String> {
        match self.get(capability) {
            Availability::Available => Ok(()),
            Availability::Unavailable(reason) => Err(reason),
        }
    }
}
