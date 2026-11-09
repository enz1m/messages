use iced::Task;
use messages_core::ChatMessage;

use super::{App, Message};

impl App {
    pub fn search_hits(&self) -> Vec<&ChatMessage> {
        let query = self.search.trim().to_lowercase();
        if query.is_empty() {
            return Vec::new();
        }
        self.current_room()
            .map(|room| {
                room.readable_messages()
                    .filter(|message| {
                        !message.deleted && message.body.to_lowercase().contains(&query)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(super) fn step_search(&mut self, direction: i8) -> Task<Message> {
        let hits = self.search_hits();
        if hits.is_empty() {
            self.search_hit = None;
            return Task::none();
        }
        let index = hits
            .iter()
            .position(|message| Some(&message.id) == self.search_hit.as_ref())
            .unwrap_or(0);
        let next = if direction < 0 {
            (index + hits.len() - 1) % hits.len()
        } else {
            (index + 1) % hits.len()
        };
        self.search_hit = Some(hits[next].id.clone());
        self.reveal_search_hit()
    }

    pub(super) fn reveal_search_hit(&mut self) -> Task<Message> {
        let Some(key) = self.selected.clone() else {
            return Task::none();
        };
        let Some(index) = self.store.room(&key).and_then(|r| {
            r.readable_messages()
                .position(|m| Some(&m.id) == self.search_hit.as_ref())
        }) else {
            return Task::none();
        };
        let _ = self.prepare_transcript(false);
        if let Some(p) = self.scroll_positions.get_mut(&key) {
            p.layout.follow = false;
            p.y = (p.layout.model.offset_of(index + 1) - p.layout.model.height_of(0)).max(0.0);
            return super::transcript::scroll_to(&key, p.y);
        }
        Task::none()
    }
}
