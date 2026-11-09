use std::time::{Duration, Instant};

use iced::Task;
use messages_core::{ConnectionState, RoomKey};
use messages_protocols::MessageAction;

use super::{App, Message};

impl App {
    pub(super) fn restore_signal_read_state(&mut self) {
        if self.demo {
            return;
        }
        let markers: Vec<_> = self
            .store
            .conversations
            .iter()
            .filter(|room| {
                self.store.accounts.iter().any(|account| {
                    account.id == room.key.account && account.protocol.as_str() == "signal"
                })
            })
            .filter_map(|room| {
                room.messages
                    .iter()
                    .filter(|message| {
                        message.own
                            && !message.id.starts_with("local:")
                            && matches!(
                                message.delivery,
                                messages_core::Delivery::Sent
                                    | messages_core::Delivery::Delivered
                                    | messages_core::Delivery::Read
                            )
                    })
                    .map(|message| message.timestamp.timestamp_millis())
                    .max()
                    .map(|through| (room.key.clone(), through))
            })
            .filter(|(key, through)| {
                !self
                    .store
                    .chat_states
                    .iter()
                    .any(|state| &state.key == key && state.read_through >= *through)
            })
            .collect();
        if !markers.is_empty() {
            for (key, through) in markers {
                self.store.mark_chat_read(&key, through);
            }
            self.changed();
        }
    }

    pub(super) fn presence(&self, key: RoomKey, action: MessageAction) -> Task<Message> {
        if !self.store.accounts.iter().any(|account| {
            account.id == key.account && account.connection == ConnectionState::Online
        }) || self
            .store
            .capabilities(&key)
            .require(action.capability())
            .is_err()
        {
            return Task::none();
        }
        let Some(adapter) = self.adapters.get(&key.account).cloned() else {
            return Task::none();
        };
        let session = self.session(key.account);
        let target = key.clone();
        let operation = action.clone();
        self.track_job(
            super::jobs::Kind::Presence,
            async move { adapter.action(&target, operation).await },
            move |result| {
                if result.is_err()
                    && let MessageAction::Read { message } = &action
                {
                    session.wrap(Message::PresenceReadFailed(key.clone(), message.clone()))
                } else {
                    session.wrap(Message::PresenceFinished)
                }
            },
        )
    }

    pub(super) fn notification_read_receipts(
        &mut self,
        key: &RoomKey,
        through: i64,
    ) -> Task<Message> {
        if !self.preferences.account(key.account).read_receipts {
            return Task::none();
        }
        let mut by_sender = std::collections::HashMap::new();
        if let Some(room) = self.store.room(key) {
            for message in &room.messages {
                if !message.own
                    && !message.deleted
                    && message.timestamp.timestamp_millis() <= through
                {
                    by_sender.insert(message.sender.clone(), message.id.clone());
                }
            }
        }
        let mut tasks = Vec::new();
        for (sender, message) in by_sender {
            let target = (key.clone(), sender);
            if self.read_sent.get(&target) != Some(&message) {
                tasks.push(self.presence(
                    key.clone(),
                    MessageAction::Read {
                        message: message.clone(),
                    },
                ));
                self.read_sent.insert(target, message);
            }
        }
        Task::batch(tasks)
    }

    pub(super) fn composer_typing(&mut self) -> Task<Message> {
        if self.stopping() {
            return Task::none();
        }
        let Some(key) = self.selected.clone() else {
            return Task::none();
        };
        if !self.window_focused
            || !self.preferences.account(key.account).typing_indicators
            || self.editing.is_some()
        {
            return Task::none();
        }
        let active = !self.editor.text().trim().is_empty();
        let now = Instant::now();
        if active {
            let entry = self
                .typing_sent
                .entry(key.clone())
                .or_insert((now - Duration::from_secs(5), now));
            entry.1 = now;
            if now.duration_since(entry.0) < Duration::from_secs(4) {
                return Task::none();
            }
            entry.0 = now;
        } else if self.typing_sent.remove(&key).is_none() {
            return Task::none();
        }
        self.presence(key, MessageAction::Typing { active })
    }

    pub(super) fn stop_typing(&mut self, key: &RoomKey) -> Task<Message> {
        if self.stopping() {
            return Task::none();
        }
        if self.typing_sent.remove(key).is_none() {
            return Task::none();
        }
        self.presence(key.clone(), MessageAction::Typing { active: false })
    }

    pub(super) fn presence_tick(&mut self) -> Task<Message> {
        if self.stopping() {
            return Task::none();
        }
        self.typing
            .retain(|_, (_, at)| at.elapsed() < Duration::from_secs(8));
        let stopped: Vec<_> = self
            .typing_sent
            .iter()
            .filter(|(_, (_, at))| at.elapsed() >= Duration::from_secs(4))
            .map(|(key, _)| key.clone())
            .collect();
        let mut tasks: Vec<_> = stopped.iter().map(|key| self.stop_typing(key)).collect();
        if self.window_focused
            && (self.window_size.width >= 760.0 || self.show_detail)
            && let Some(room) = self.current_room()
        {
            let key = room.key.clone();
            if !self.preferences.account(key.account).read_receipts {
                return Task::batch(tasks);
            }
            let mut messages = std::collections::HashMap::new();
            for message in &room.messages {
                if !message.own && !message.deleted {
                    messages.insert(message.sender.clone(), message.id.clone());
                }
            }
            for (author, message) in messages {
                let target = (key.clone(), author);
                if self.read_sent.get(&target) != Some(&message) {
                    tasks.push(self.presence(
                        key.clone(),
                        MessageAction::Read {
                            message: message.clone(),
                        },
                    ));
                    self.read_sent.insert(target, message);
                }
            }
        }
        Task::batch(tasks)
    }
}
