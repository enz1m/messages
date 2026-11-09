use iced::{Task, widget::scrollable::Viewport};
use messages_core::RoomKey;
use messages_protocols::{AdapterError, HistoryPage};

use super::{App, Message};

#[derive(Default)]
pub struct Position {
    pub(crate) height: f32,
    pub(crate) y: f32,
    pub(crate) viewport_height: f32,
    pub(crate) layout: super::transcript::Layout,
}

pub fn transcript_id(key: &RoomKey) -> iced::widget::Id {
    iced::widget::Id::from(format!("messages-transcript-{}-{}", key.account, key.room))
}

impl Position {
    pub fn is_away_from_bottom(&self) -> bool {
        self.height - self.viewport_height - self.y > 48.0
    }
}

impl App {
    pub(super) fn jump_to_bottom(&mut self, key: &RoomKey) -> Task<Message> {
        if let Some(position) = self.scroll_positions.get_mut(key) {
            position.layout.follow = true;
            position.y = (position.layout.model.total_height() - position.viewport_height).max(0.0);
            return super::transcript::scroll_to(key, position.y);
        }
        Task::none()
    }

    pub(super) fn load_older(&mut self, key: RoomKey) -> Task<Message> {
        if self.loading || self.history_busy.contains(&key) {
            return Task::none();
        }
        let Some(room) = self.store.room(&key).filter(|room| room.has_older) else {
            return Task::none();
        };
        let before = room.pagination.clone();
        let Some(adapter) = self.adapters.get(&key.account).cloned() else {
            self.history_errors.insert(
                key,
                "Reconnect this account in Settings to load older messages.".into(),
            );
            return Task::none();
        };
        self.history_busy.insert(key.clone());
        self.history_errors.remove(&key);
        let session = self.session(key.account);
        let requested_key = key.clone();
        let requested_before = before.clone();
        self.track_job(
            super::jobs::Kind::History,
            async move {
                adapter
                    .history_page(&requested_key, requested_before.as_deref())
                    .await
            },
            move |result| session.wrap(Message::HistoryLoaded(key.clone(), before.clone(), result)),
        )
    }

    pub(super) fn history_loaded(
        &mut self,
        key: RoomKey,
        before: Option<String>,
        result: Result<HistoryPage, AdapterError>,
    ) -> Task<Message> {
        if !self.history_busy.remove(&key) {
            return Task::none();
        }
        let Some(room) = self.store.room(&key) else {
            return Task::none();
        };
        if room.pagination != before {
            return Task::none();
        }
        match result {
            Ok(page) if page.next.is_some() && page.next == before => {
                self.history_errors.insert(
                    key,
                    "The service returned the same history page. Try again shortly.".into(),
                );
            }
            Ok(page) => {
                let _ = self.store.merge_messages(&key, page.messages);
                if let Some(room) = self.store.room_mut(&key) {
                    room.has_older = page.next.is_some();
                    room.pagination = page.next;
                }
                self.changed();
                return self.save();
            }
            Err(error) => {
                self.history_errors.insert(key, error.to_string());
            }
        }
        Task::none()
    }

    pub(super) fn transcript_scrolled(
        &mut self,
        key: RoomKey,
        viewport: Viewport,
    ) -> Task<Message> {
        let position = self.scroll_positions.entry(key.clone()).or_default();
        position.height = viewport.content_bounds().height;
        position.viewport_height = viewport.bounds().height;
        position.y = viewport.absolute_offset().y;
        position.layout.follow = position.height - position.viewport_height - position.y <= 48.0;
        Task::none()
    }
}
