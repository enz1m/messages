//! Network adapters for Messages. A registry owns factories; the UI consumes only
//! this contract and protocol-neutral domain values.

pub mod adapter;
pub mod bridge;
pub mod calls;
pub mod http;
pub mod login;
pub mod matrix;
mod matrix_media;
pub mod matrix_timeline;
pub mod media;
pub mod registry;
pub mod signal;
mod signal_contacts;
mod signal_events;
mod signal_login;
mod signal_media;
mod sse;

pub use adapter::{
    Adapter, AdapterError, ConnectionConfig, HistoryPage, ManagedConnection, MessageAction,
    SyncBatch, TypingUpdate,
};
pub use registry::{ProtocolDescriptor, Registry};
