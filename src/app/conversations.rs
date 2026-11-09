use iced::{Task, widget::text_editor};
use messages_core::RoomKey;

use super::{App, Message};

impl App {
    pub(super) fn delete_conversation(&mut self, key: RoomKey) -> Task<Message> {
        if self.loading
            || self.persistence_blocked
            || self.file_busy.contains(&key)
            || self.attachment_operations_pending(&key)
        {
            return Task::none();
        }
        let draft = self
            .store
            .room(&key)
            .map(|room| room.draft_attachments.clone())
            .unwrap_or_default();
        if let Err(error) = self.store.delete_chat(&key, chrono::Utc::now()) {
            self.notice = Some(error.to_string().into());
            return Task::none();
        }
        self.withdraw_room_notification(&key);
        let stop = self.stop_typing(&key);
        self.typing.retain(|(room, _), _| room != &key);
        self.scroll_positions.remove(&key);
        self.history_busy.remove(&key);
        self.history_errors.remove(&key);
        self.previews.retain(|preview, _| preview.room != key);
        self.preview_tasks.retain(|preview, _| preview.room != key);
        self.visible_previews.retain(|preview| preview.room != key);
        if self.selected.as_ref() == Some(&key) {
            self.selected = self
                .store
                .conversations
                .first()
                .map(|room| room.key.clone());
            self.editor = text_editor::Content::with_text(
                self.current_room()
                    .map(|room| room.draft.as_str())
                    .unwrap_or(""),
            );
            self.editing = None;
            self.show_detail = false;
        }
        self.changed();
        self.file_reclaims.extend(
            draft
                .into_iter()
                .map(|file| (self.revision, key.clone(), file)),
        );
        Task::batch([stop, self.close_dialog(), self.save()])
    }

    fn attachment_operations_pending(&self, key: &RoomKey) -> bool {
        self.file_operations
            .iter()
            .any(|operation| match operation {
                super::files::Purpose::Attach(_, room)
                | super::files::Purpose::Save(_, room, ..)
                | super::files::Purpose::Open(_, room, ..) => room == key,
            })
    }
}
