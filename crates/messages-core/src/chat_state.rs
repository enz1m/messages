use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{AccountId, ChatMessage, MessageStore, RoomKey, StoreError};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatState {
    pub key: RoomKey,
    pub read_through: i64,
    pub deleted_through: i64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeviceRead {
    pub account: AccountId,
    pub sender: String,
    pub timestamp: i64,
}

impl MessageStore {
    pub fn message_is_read(&self, key: &RoomKey, message: &ChatMessage) -> bool {
        self.chat_states.iter().any(|state| {
            &state.key == key && message.timestamp.timestamp_millis() <= state.read_through
        }) || self.device_reads.iter().any(|read| {
            read.account == key.account
                && read.sender == message.sender
                && read.timestamp == message.timestamp.timestamp_millis()
        })
    }

    pub fn admits_message(&self, key: &RoomKey, message: &ChatMessage) -> bool {
        self.chat_states.iter().all(|state| {
            &state.key != key || message.timestamp.timestamp_millis() > state.deleted_through
        })
    }

    pub fn chat_was_deleted(&self, key: &RoomKey) -> bool {
        self.chat_states
            .iter()
            .any(|state| &state.key == key && state.deleted_through > 0)
    }

    fn state_mut(&mut self, key: &RoomKey) -> &mut ChatState {
        let index = self
            .chat_states
            .iter()
            .position(|state| &state.key == key)
            .unwrap_or_else(|| {
                self.chat_states.push(ChatState {
                    key: key.clone(),
                    read_through: 0,
                    deleted_through: 0,
                });
                self.chat_states.len() - 1
            });
        &mut self.chat_states[index]
    }

    pub fn mark_chat_read(&mut self, key: &RoomKey, through: i64) {
        let Some(room) = self.room(key) else {
            return;
        };
        let cleared = room
            .messages
            .iter()
            .rev()
            .filter(|message| {
                !message.own && !message.deleted && !self.message_is_read(key, message)
            })
            .take(room.unread as usize)
            .filter(|message| message.timestamp.timestamp_millis() <= through)
            .count();
        if let Some(room) = self.room_mut(key) {
            room.unread = room
                .unread
                .saturating_sub(u32::try_from(cleared).unwrap_or(u32::MAX));
            if room
                .messages
                .last()
                .is_none_or(|message| message.timestamp.timestamp_millis() <= through)
            {
                room.unread = 0;
            }
        }
        let state = self.state_mut(key);
        state.read_through = state.read_through.max(through);
    }

    pub fn apply_device_read(&mut self, read: DeviceRead) {
        if read.timestamp <= 0
            || !self
                .accounts
                .iter()
                .any(|account| account.id == read.account)
            || self.device_reads.contains(&read)
        {
            return;
        }
        let keys: Vec<_> = self
            .conversations
            .iter()
            .filter(|room| {
                room.key.account == read.account
                    && room
                        .messages
                        .iter()
                        .rev()
                        .filter(|message| {
                            !message.own
                                && !message.deleted
                                && !self.message_is_read(&room.key, message)
                        })
                        .take(room.unread as usize)
                        .any(|message| {
                            message.sender == read.sender
                                && message.timestamp.timestamp_millis() == read.timestamp
                        })
            })
            .map(|room| room.key.clone())
            .collect();
        for key in keys {
            if let Some(room) = self.room_mut(&key) {
                room.unread = room.unread.saturating_sub(1);
            }
        }
        self.device_reads.push(read);
    }

    pub fn delete_chat(&mut self, key: &RoomKey, now: DateTime<Utc>) -> Result<(), StoreError> {
        let room = self.room(key).ok_or(StoreError::MissingRoom)?;
        if self.outbox.iter().any(|item| &item.request.room == key) {
            return Err(StoreError::AlreadySending);
        }
        let through = room
            .messages
            .last()
            .map(|message| message.timestamp.timestamp_millis())
            .unwrap_or(0)
            .max(now.timestamp_millis());
        self.conversations.retain(|room| &room.key != key);
        self.revisions.retain(|revision| !revision.belongs_to(key));
        let state = self.state_mut(key);
        state.deleted_through = state.deleted_through.max(through);
        state.read_through = state.read_through.max(through);
        Ok(())
    }
}
