//! Protocol-neutral messaging state. Network and presentation code consume the
//! same capability decisions and account-scoped identities.

pub mod capabilities;
pub mod changes;
pub mod chat_state;
pub use changes::{ChangeKind, MessageChange, MessageRevision};
pub use chat_state::{ChatState, DeviceRead};
pub mod fixture;
mod merge_index;
pub mod model;
pub mod outbox;
pub mod store;

pub use capabilities::{Availability, Capabilities, Capability};
pub use model::{
    Account, AccountId, Attachment, ChatMessage, ConnectionState, Conversation, Delivery,
    Encryption, MessageContentState, ProtocolId, Reaction, RoomKey, RoomKind,
};
pub use outbox::{
    MAX_ATTACHMENT_BYTES, MAX_ATTACHMENTS, OutboxItem, OutboxState, OutgoingAttachment, SendError,
    SendRequest,
};
pub use store::{MessageStore, StoreError};
