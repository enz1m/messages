use iced::Task;
use messages_core::{AccountId, RoomKey};

use super::{App, Message};

pub(crate) const MUTE_DURATIONS: &[(&str, Option<i64>)] = &[
    ("Mute for 15 minutes", Some(900)),
    ("Mute for 1 hour", Some(3600)),
    ("Mute for 8 hours", Some(28800)),
    ("Mute for 24 hours", Some(86400)),
    ("Mute until I turn them back on", None),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BubbleSide {
    Sent,
    Received,
}

#[derive(Clone, Debug)]
pub enum Preference {
    TextSize(u8),
    BubbleColor(String),
    ReceivedBubbleColor(String),
    BubbleRadius(f32),
    GroupMessages(bool),
    Notifications(bool),
    NotificationPreview(bool),
    AccountRead(AccountId, bool),
    AccountTyping(AccountId, bool),
    AccountNotifications(AccountId, bool),
}

impl App {
    pub(super) fn set_preference(&mut self, value: Preference) -> Task<Message> {
        match value {
            Preference::TextSize(size) => self.preferences.text_size = size.clamp(11, 22),
            Preference::BubbleColor(color) => self.preferences.bubble_color = color,
            Preference::ReceivedBubbleColor(color) => {
                self.preferences.received_bubble_color = color
            }
            Preference::BubbleRadius(radius) => {
                self.preferences.bubble_radius = radius.clamp(4.0, 20.0)
            }
            Preference::GroupMessages(value) => self.preferences.group_messages = value,
            Preference::Notifications(value) => {
                self.preferences.notifications = value;
                if !value {
                    let keys: Vec<_> = self
                        .store
                        .conversations
                        .iter()
                        .map(|r| r.key.clone())
                        .collect();
                    for key in keys {
                        self.withdraw_room_notification(&key);
                    }
                }
            }
            Preference::NotificationPreview(value) => {
                self.preferences.notification_preview = value;
                if !value {
                    let keys: Vec<_> = self
                        .store
                        .conversations
                        .iter()
                        .map(|r| r.key.clone())
                        .collect();
                    for key in keys {
                        self.withdraw_room_notification(&key);
                    }
                }
            }
            Preference::AccountRead(id, value) => {
                self.preferences.account_mut(id).read_receipts = value;
                self.read_sent.retain(|(key, _), _| key.account != id);
            }
            Preference::AccountTyping(id, value) => {
                self.preferences.account_mut(id).typing_indicators = value;
                self.changed();
                if !value {
                    let keys: Vec<_> = self
                        .typing_sent
                        .keys()
                        .filter(|key| key.account == id)
                        .cloned()
                        .collect();
                    return Task::batch(keys.iter().map(|key| self.stop_typing(key)));
                }
                return Task::none();
            }
            Preference::AccountNotifications(id, value) => {
                self.preferences.account_mut(id).notifications = value;
                if !value {
                    let keys: Vec<_> = self
                        .store
                        .conversations
                        .iter()
                        .filter(|r| r.key.account == id)
                        .map(|r| r.key.clone())
                        .collect();
                    for key in keys {
                        self.withdraw_room_notification(&key);
                    }
                }
            }
        }
        self.changed();
        Task::none()
    }

    pub(super) fn mute_for(&mut self, key: RoomKey, seconds: Option<i64>) -> Task<Message> {
        if let Some(room) = self.store.room_mut(&key) {
            room.muted = seconds != Some(0);
            room.muted_until = seconds
                .filter(|value| *value > 0)
                .map(|value| chrono::Utc::now() + chrono::Duration::seconds(value));
            self.changed();
        }
        if seconds != Some(0) {
            self.withdraw_room_notification(&key);
        }
        self.popup = None;
        Task::none()
    }

    pub(super) fn expire_mutes(&mut self) {
        let now = chrono::Utc::now();
        let mut changed = false;
        for room in &mut self.store.conversations {
            if room.muted_until.is_some_and(|until| until <= now) {
                room.muted = false;
                room.muted_until = None;
                changed = true;
            }
        }
        if changed {
            self.changed();
        }
    }

    pub(super) fn select_message(&mut self, key: RoomKey, id: String) {
        if self.selected.as_ref() != Some(&key)
            || !self
                .store
                .room(&key)
                .is_some_and(|room| room.messages.iter().any(|m| m.id == id && !m.deleted))
        {
            return;
        }
        if !self.selected_messages.insert(id.clone()) {
            self.selected_messages.remove(&id);
        }
    }

    pub(super) fn copy_selected(&mut self) -> Task<Message> {
        let text = self
            .current_room()
            .map(|room| {
                room.messages
                    .iter()
                    .filter(|m| self.selected_messages.contains(&m.id))
                    .map(|m| format!("{}: {}", m.sender_name, m.body))
                    .collect::<Vec<_>>()
                    .join("\n\n")
            })
            .unwrap_or_default();
        self.update(Message::Copy(text))
    }
}
