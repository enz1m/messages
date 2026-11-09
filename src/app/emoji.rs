use super::{App, Message, Popup, lifecycle};
use enzui_iced_widgets::emoji_picker::EmojiPickerEvent;
use iced::Task;
use messages_core::RoomKey;
use uuid::Uuid;

pub fn picker_prefix(generation: Uuid) -> String {
    format!("messages-emoji-picker-{generation}")
}

#[derive(Clone, Debug)]
pub struct Event {
    pub generation: Uuid,
    pub session: lifecycle::Session,
    pub room: RoomKey,
    pub target: Popup,
    pub event: EmojiPickerEvent,
}

impl App {
    pub(super) fn emoji_event(&mut self, event: Event) -> Task<Message> {
        if event.generation != self.emoji_generation
            || !self.session_is_current(event.session)
            || self.selected.as_ref() != Some(&event.room)
            || self.popup.as_ref() != Some(&event.target)
        {
            return Task::none();
        }
        match event.event {
            EmojiPickerEvent::Query(query) => self.emoji_query = query,
            EmojiPickerEvent::Category(category) => {
                self.emoji_category = category;
                self.emoji_query.clear();
            }
            EmojiPickerEvent::Picked(emoji) => {
                self.emoji_recents.record(&emoji);
                self.preferences.emoji_recents = self.emoji_recents.list().to_vec();
                self.changed();
                return self.reduce(match event.target {
                    Popup::Emoji => Message::InsertEmoji(emoji),
                    Popup::Reaction(_, _) => Message::PickReaction(emoji),
                    _ => return Task::none(),
                });
            }
        }
        Task::none()
    }
}
