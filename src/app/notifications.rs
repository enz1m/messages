use super::{App, Message, lifecycle::Session};
use crate::platform::notifications::{Event, Notice};
use iced::Task;
use messages_core::{AccountId, Capability, RoomKey};
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct Binding {
    pub session: Session,
    pub room: RoomKey,
    pub through: i64,
    pub message: String,
}

impl App {
    pub(super) fn test_notification(&mut self) {
        if !self.preferences.notifications {
            return;
        }
        if !self
            .notifications
            .as_ref()
            .is_some_and(|client| client.post_test())
        {
            self.notice=Some("Desktop notifications are not ready. Check your system notification settings and try again.".into());
        }
    }

    pub(super) fn retry_notifications(&mut self) -> Task<Message> {
        if self.notifications.is_none()
            && !self.notification_connecting
            && self.preferences.notifications
            && self.notification_retry.elapsed().as_secs() >= 60
        {
            self.notification_retry = std::time::Instant::now();
            self.notification_connecting = true;
            return self.track_job(
                super::jobs::Kind::Notifications,
                async {
                    tokio::time::timeout(
                        std::time::Duration::from_secs(10),
                        crate::platform::notifications::Client::connect(),
                    )
                    .await
                    .map_err(|_| "Notifications timed out".to_owned())?
                },
                Message::NotificationsReady,
            );
        }
        Task::none()
    }

    pub(super) fn withdraw_room_notification(&mut self, key: &RoomKey) {
        let ids: Vec<_> = self
            .notification_bindings
            .iter()
            .filter(|(_, binding)| &binding.room == key)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.notification_bindings.remove(&id);
            self.notification_activations.remove(&id);
            if let Some(client) = &self.notifications {
                client.close(id);
            }
        }
        self.notification_pending
            .retain(|_, binding| &binding.room != key);
    }

    pub(super) fn baseline_notifications(&mut self, account: AccountId) {
        self.notification_seen.extend(
            self.store
                .conversations
                .iter()
                .filter(|room| room.key.account == account)
                .filter_map(|room| {
                    room.readable_messages()
                        .rev()
                        .find(|message| !message.own && !message.deleted)
                        .map(|message| (room.key.clone(), message.id.clone()))
                }),
        );
    }

    pub(super) fn quiet_foreground_notifications(&mut self) {
        let accounts: Vec<_> = self.store.accounts.iter().map(|a| a.id).collect();
        for account in accounts {
            self.baseline_notifications(account);
        }
        let rooms: std::collections::HashSet<_> = self
            .notification_bindings
            .values()
            .chain(self.notification_pending.values())
            .map(|binding| binding.room.clone())
            .collect();
        for room in rooms {
            self.withdraw_room_notification(&room);
        }
    }

    pub(super) fn notify_unread(&mut self, account: AccountId) {
        if self.stopping() {
            return;
        }
        let Some(since) = self.notification_since.get(&account).copied() else {
            self.notification_since.insert(account, chrono::Utc::now());
            self.baseline_notifications(account);
            return;
        };
        let quiet = self.window_focused
            || self.demo
            || !self.preferences.notifications
            || !self.preferences.account(account).notifications;
        for room in self
            .store
            .conversations
            .iter()
            .filter(|room| room.key.account == account)
        {
            if room.unread == 0 || room.muted {
                continue;
            }
            let Some(message) = room
                .readable_messages()
                .rev()
                .find(|m| !m.own && !m.deleted && !self.store.message_is_read(&room.key, m))
            else {
                continue;
            };
            if self.notification_seen.get(&room.key) == Some(&message.id) {
                continue;
            }
            if quiet || message.timestamp <= since {
                self.notification_seen
                    .insert(room.key.clone(), message.id.clone());
                continue;
            }
            let Some(client) = &self.notifications else {
                continue;
            };
            if self.notification_bindings.len() + self.notification_pending.len() >= 128 {
                continue;
            }
            let ticket = Uuid::new_v4();
            let body = if self.preferences.notification_preview {
                room.preview().chars().take(240).collect()
            } else {
                format!(
                    "{} unread message{}",
                    room.unread,
                    if room.unread == 1 { "" } else { "s" }
                )
            };
            let session = self.session(account);
            if client.post(Notice {
                ticket,
                room: format!("{}:{}", account, room.key.room),
                title: room.title.chars().take(60).collect(),
                body,
                reply: self
                    .store
                    .capabilities(&room.key)
                    .supports(Capability::SendText),
            }) {
                self.notification_pending
                    .retain(|_, binding| binding.room != room.key);
                self.notification_pending.insert(
                    ticket,
                    Binding {
                        session,
                        room: room.key.clone(),
                        through: message.timestamp.timestamp_millis(),
                        message: message.id.clone(),
                    },
                );
                self.notification_seen
                    .insert(room.key.clone(), message.id.clone());
            }
        }
    }

    pub(super) fn notification_event(&mut self, event: Event) -> Task<Message> {
        if self.stop_mode.is_some() && matches!(&event, Event::Action(..) | Event::Reply(..)) {
            return Task::none();
        }
        match event {
            Event::TestPosted => {}
            Event::TestFailed => {
                self.notice=Some("The test notification could not be shown. Check your system notification permissions.".into());
            }
            Event::Closed(id) => {
                self.notification_bindings.remove(&id);
                self.notification_activations.remove(&id);
            }
            Event::Activation(id, token) => {
                if self.notification_bindings.contains_key(&id) {
                    self.notification_activations.insert(id, token);
                }
            }
            Event::Posted(ticket, id) => {
                if let Some(binding) = self.notification_pending.remove(&ticket) {
                    if self.session_is_current(binding.session) {
                        self.notification_bindings.insert(id, binding);
                    } else if let Some(client) = &self.notifications {
                        client.close(id);
                    }
                } else if let Some(client) = &self.notifications {
                    client.close(id);
                }
            }
            Event::Action(id, action) => {
                let Some(binding) = self
                    .notification_bindings
                    .get(&id)
                    .cloned()
                    .filter(|b| self.session_is_current(b.session))
                else {
                    return Task::none();
                };
                match action.as_str() {
                    "default" => {
                        let focus = if let Some(context) = self.notification_activations.remove(&id)
                        {
                            let mut context = Some(context);
                            iced::window::latest().then(move |window| {
                                match (window, context.take()) {
                                    (Some(window), Some(context)) => context
                                        .activate(window, std::time::Instant::now())
                                        .map(|_| Message::ActivationFinished),
                                    _ => Task::none(),
                                }
                            })
                        } else {
                            iced::window::latest().and_then(iced::window::gain_focus)
                        };
                        return Task::batch([self.update(Message::Select(binding.room)), focus]);
                    }
                    "mark-read" => {
                        self.withdraw_room_notification(&binding.room);
                        self.store.mark_chat_read(&binding.room, binding.through);
                        self.changed();
                        return Task::batch([
                            self.notification_read_receipts(&binding.room, binding.through),
                            self.save(),
                        ]);
                    }
                    _ => return Task::none(),
                }
            }
            Event::Reply(id, text) => {
                let Some(binding) = self
                    .notification_bindings
                    .get(&id)
                    .cloned()
                    .filter(|b| self.session_is_current(b.session))
                else {
                    return Task::none();
                };
                if self.loading
                    || self.closing.is_some()
                    || self.persistence_blocked
                    || text.trim().is_empty()
                    || text.len() > 4096
                {
                    return Task::none();
                }
                let Some(_) = self.store.room(&binding.room) else {
                    return Task::none();
                };
                self.notification_bindings.remove(&id);
                self.notification_activations.remove(&id);
                let room = self
                    .store
                    .room(&binding.room)
                    .expect("validated notification room");
                let draft = room.draft.clone();
                let files = room.draft_attachments.clone();
                let reply = room.reply_to.clone();
                let reply_to = self
                    .store
                    .capabilities(&binding.room)
                    .supports(Capability::Reply)
                    .then_some(binding.message);
                if let Some(room) = self.store.room_mut(&binding.room) {
                    room.reply_to = reply_to;
                }
                let queued = self
                    .store
                    .queue_message(&binding.room, text, chrono::Utc::now());
                if let Some(room) = self.store.room_mut(&binding.room) {
                    room.draft = draft;
                    room.draft_attachments = files;
                    room.reply_to = reply;
                }
                if let Some(client) = &self.notifications {
                    client.close(id);
                }
                match queued {
                    Ok(request) => {
                        self.store.mark_chat_read(&binding.room, binding.through);
                        self.changed();
                        return Task::batch([
                            self.notification_read_receipts(&binding.room, binding.through),
                            self.stage_send(request.transaction),
                        ]);
                    }
                    Err(error) => self.notice = Some(error.to_string().into()),
                }
            }
            Event::Failed(ticket) => {
                if let Some(binding) = self.notification_pending.remove(&ticket) {
                    self.notification_seen.remove(&binding.room);
                }
            }
            Event::Unavailable => {
                self.notifications = None;
                self.notification_retry = std::time::Instant::now();
                self.notification_bindings.clear();
                self.notification_activations.clear();
                self.notification_pending.clear();
                self.notification_seen.clear();
            }
        }
        Task::none()
    }
}
