use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    Account, Capabilities, Capability, ChatMessage, Conversation, Delivery, OutboxItem,
    OutboxState, RoomKey, SendError, SendRequest,
};

pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MessageStore {
    pub accounts: Vec<Account>,
    pub conversations: Vec<Conversation>,
    pub outbox: Vec<OutboxItem>,
    #[serde(default)]
    pub revisions: Vec<crate::MessageRevision>,
    #[serde(default)]
    pub chat_states: Vec<crate::ChatState>,
    #[serde(default)]
    pub device_reads: Vec<crate::DeviceRead>,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum StoreError {
    #[error("This conversation is no longer available.")]
    MissingRoom,
    #[error("This account is no longer available.")]
    MissingAccount,
    #[error("Write a message first.")]
    EmptyMessage,
    #[error("This message is too long. Split it into smaller messages.")]
    MessageTooLong,
    #[error("{0}")]
    Unsupported(String),
    #[error("The queued message is no longer available.")]
    MissingSend,
    #[error("Attach at most 10 files, up to 100 MB in total.")]
    AttachmentLimit,
    #[error("This message is already being sent.")]
    AlreadySending,
}

impl MessageStore {
    pub fn room(&self, key: &RoomKey) -> Option<&Conversation> {
        self.conversations.iter().find(|room| &room.key == key)
    }

    pub fn room_mut(&mut self, key: &RoomKey) -> Option<&mut Conversation> {
        self.conversations.iter_mut().find(|room| &room.key == key)
    }

    pub fn capabilities(&self, key: &RoomKey) -> Capabilities {
        let Some(account) = self.accounts.iter().find(|a| a.id == key.account) else {
            return Capabilities::default();
        };
        self.room(key)
            .map(|room| account.capabilities.restricted_by(&room.capabilities))
            .unwrap_or_default()
    }

    /// Optimistic insertion and queue admission are one state transition. The
    /// caller persists this store before dispatching the returned request.
    pub fn queue_message(
        &mut self,
        key: &RoomKey,
        body: String,
        now: DateTime<Utc>,
    ) -> Result<SendRequest, StoreError> {
        self.queue_with_attachments(key, body, Vec::new(), now)
    }

    pub fn queue_with_attachments(
        &mut self,
        key: &RoomKey,
        body: String,
        attachments: Vec<crate::OutgoingAttachment>,
        now: DateTime<Utc>,
    ) -> Result<SendRequest, StoreError> {
        if self.room(key).is_none() {
            return Err(StoreError::MissingRoom);
        }
        let capabilities = self.capabilities(key);
        capabilities
            .require(Capability::SendText)
            .map_err(StoreError::Unsupported)?;
        if !attachments.is_empty() {
            capabilities
                .require(Capability::AttachFiles)
                .map_err(StoreError::Unsupported)?;
        }
        let total = attachments
            .iter()
            .try_fold(0_u64, |sum, file| sum.checked_add(file.bytes));
        if attachments.len() > crate::MAX_ATTACHMENTS
            || total.is_none_or(|bytes| bytes > crate::MAX_ATTACHMENT_BYTES)
        {
            return Err(StoreError::AttachmentLimit);
        }
        if body.trim().is_empty() && attachments.is_empty() {
            return Err(StoreError::EmptyMessage);
        }
        if body.len() > MAX_MESSAGE_BYTES {
            return Err(StoreError::MessageTooLong);
        }
        let account = self
            .accounts
            .iter()
            .find(|a| a.id == key.account)
            .ok_or(StoreError::MissingAccount)?;
        let sender = account.user_id.clone();
        let transaction = Uuid::new_v4();
        let room = self.room_mut(key).ok_or(StoreError::MissingRoom)?;
        if room.reply_to.is_some() {
            capabilities
                .require(Capability::Reply)
                .map_err(StoreError::Unsupported)?;
        }
        let request = SendRequest {
            transaction,
            room: key.clone(),
            body: body.clone(),
            reply_to: room.reply_to.take(),
            attachments: attachments.clone(),
        };
        room.messages.push(ChatMessage {
            content_state: Some(crate::MessageContentState::Available),
            id: format!("local:{transaction}"),
            transaction: Some(transaction),
            sender,
            sender_name: "You".into(),
            own: true,
            body,
            timestamp: now,
            delivery: Delivery::Queued,
            edited: false,
            deleted: false,
            reply_to: request.reply_to.clone(),
            reactions: Vec::new(),
            attachments: attachments
                .iter()
                .map(|file| crate::Attachment {
                    id: format!("local:{}", file.id),
                    name: file.name.clone(),
                    mime: file.mime.clone(),
                    bytes: file.bytes,
                    remote_url: Some(file.path.to_string_lossy().into_owned()),
                })
                .collect(),
        });
        room.draft.clear();
        room.draft_attachments.clear();
        self.outbox.push(OutboxItem {
            request: request.clone(),
            created_at: now,
            attempts: 0,
            state: OutboxState::Queued,
        });
        Ok(request)
    }

    pub fn begin_send(&mut self, transaction: Uuid) -> Result<SendRequest, StoreError> {
        let item = self
            .outbox
            .iter_mut()
            .find(|item| item.request.transaction == transaction)
            .ok_or(StoreError::MissingSend)?;
        if item.state == OutboxState::Sending {
            return Err(StoreError::AlreadySending);
        }
        item.state = OutboxState::Sending;
        item.attempts = item.attempts.saturating_add(1);
        let request = item.request.clone();
        if let Some(message) = self.local_message_mut(&request.room, transaction) {
            message.delivery = Delivery::Sending;
        }
        Ok(request)
    }

    pub fn acknowledge(&mut self, key: &RoomKey, transaction: Uuid, remote_id: String) {
        if let Some(room) = self.room_mut(key) {
            // A sync echo can race the send response. Remove its duplicate while
            // retaining the optimistic row's stable transaction and position.
            let local = room
                .messages
                .iter()
                .position(|m| m.transaction == Some(transaction));
            if let Some(index) = local {
                let receipt = room
                    .messages
                    .iter()
                    .find(|m| m.id == remote_id)
                    .map(|m| m.delivery.clone());
                room.messages[index].id = remote_id.clone();
                if room.messages[index].delivery.receipt_rank() < 1 {
                    room.messages[index].delivery = Delivery::Sent;
                }
                if let Some(receipt) = receipt
                    && receipt.receipt_rank() > room.messages[index].delivery.receipt_rank()
                {
                    room.messages[index].delivery = receipt;
                }
                let mut kept = false;
                room.messages.retain(|m| {
                    if m.id != remote_id {
                        return true;
                    }
                    if m.transaction == Some(transaction) && !kept {
                        kept = true;
                        true
                    } else {
                        false
                    }
                });
            }
        }
        self.outbox
            .retain(|item| !(item.request.transaction == transaction && &item.request.room == key));
    }

    pub fn fail_send(&mut self, key: &RoomKey, transaction: Uuid, error: SendError) {
        let state = match &error {
            SendError::Rejected(reason) => OutboxState::Failed(reason.clone()),
            SendError::Uncertain => OutboxState::Uncertain,
        };
        let Some(item) = self
            .outbox
            .iter_mut()
            .find(|item| item.request.transaction == transaction && &item.request.room == key)
        else {
            return;
        };
        item.state = state;
        if let Some(message) = self.local_message_mut(key, transaction) {
            message.delivery = match error {
                SendError::Rejected(reason) => Delivery::Failed(reason),
                SendError::Uncertain => Delivery::Uncertain,
            };
        }
    }

    fn local_message_mut(&mut self, key: &RoomKey, transaction: Uuid) -> Option<&mut ChatMessage> {
        self.room_mut(key)?
            .messages
            .iter_mut()
            .find(|message| message.transaction == Some(transaction))
    }

    /// Merge remote messages and settle confirmed transactions.
    /// Later receipts, redactions and locally held outgoing attachments remain authoritative.
    pub fn merge_messages(
        &mut self,
        key: &RoomKey,
        incoming: Vec<ChatMessage>,
    ) -> Result<(), StoreError> {
        let incoming = incoming
            .into_iter()
            .filter(|message| self.admits_message(key, message))
            .collect::<Vec<_>>();
        let room = self.room_mut(key).ok_or(StoreError::MissingRoom)?;
        let mut confirmed = std::collections::HashSet::new();
        let mut index =
            (incoming.len() > 16).then(|| crate::merge_index::MergeIndex::new(&room.messages));
        for mut message in incoming {
            let position = match &index {
                Some(index) => index.find(&message),
                None => room.messages.iter().position(|old| {
                    old.id == message.id
                        || (message.transaction.is_some() && old.transaction == message.transaction)
                }),
            };
            if let Some(position) = position {
                let old = &mut room.messages[position];
                if old.content_state == Some(crate::MessageContentState::Relation) {
                    message.content_state = old.content_state;
                }
                if old.deleted {
                    message.deleted = true;
                    message.body.clear();
                    message.reply_to = None;
                    message.reactions.clear();
                    message.attachments.clear();
                }
                if old.delivery.receipt_rank() > message.delivery.receipt_rank() {
                    message.delivery = old.delivery.clone();
                }
                if old.transaction.is_some()
                    && !old.deleted
                    && message.own
                    && !old.attachments.is_empty()
                {
                    message.attachments = old.attachments.clone();
                }
                if message.transaction.is_none() {
                    message.transaction = old.transaction;
                }
                if let Some(transaction) = message.transaction {
                    confirmed.insert(transaction);
                }
                if let Some(index) = &mut index {
                    index.replace(old, &message, position);
                }
                *old = message;
            } else {
                if let Some(index) = &mut index {
                    index.insert(&message, room.messages.len());
                }
                room.messages.push(message);
            }
        }
        room.messages.sort_by_key(|message| message.timestamp);
        self.reapply_changes(key);
        self.outbox.retain(|item| {
            &item.request.room != key || !confirmed.contains(&item.request.transaction)
        });
        Ok(())
    }

    /// An interrupted non-idempotent send is never blindly re-sent at startup.
    pub fn recover_interrupted_sends(&mut self) {
        let interrupted: Vec<_> = self
            .outbox
            .iter()
            .filter(|item| item.state == OutboxState::Sending)
            .map(|item| (item.request.room.clone(), item.request.transaction))
            .collect();
        for (key, transaction) in interrupted {
            self.fail_send(&key, transaction, SendError::Uncertain);
        }
    }

    pub fn visible_rooms(
        &self,
        query: &str,
        protocol: Option<&str>,
        unread_only: bool,
        archived: bool,
    ) -> Vec<&Conversation> {
        let query = query.trim().to_lowercase();
        let mut rooms: Vec<_> = self
            .conversations
            .iter()
            .filter(|room| {
                room.archived == archived
                    && (!unread_only || room.unread > 0)
                    && protocol.is_none_or(|id| {
                        self.accounts
                            .iter()
                            .any(|a| a.id == room.key.account && a.protocol.as_str() == id)
                    })
                    && (query.is_empty()
                        || room.title.to_lowercase().contains(&query)
                        || room.messages.iter().any(|message| {
                            !message.deleted
                                && message.is_readable()
                                && message.body.to_lowercase().contains(&query)
                        }))
            })
            .collect();
        rooms.sort_by(|a, b| {
            b.pinned
                .cmp(&a.pinned)
                .then_with(|| b.updated_at().cmp(&a.updated_at()))
        });
        rooms
    }
}
