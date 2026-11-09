use iced::{Task, widget::text_editor};
use messages_core::{AccountId, Capability, Conversation};
use messages_protocols::AdapterError;

use super::{App, Message};

impl App {
    pub(super) fn start_conversation(&mut self) -> Task<Message> {
        if self.new_busy || self.loading || self.persistence_blocked || self.closing.is_some() {
            return Task::none();
        }
        let Some(id) = self.new_account else {
            return Task::none();
        };
        let Some(account) = self.store.accounts.iter().find(|account| account.id == id) else {
            return Task::none();
        };
        if let Err(reason) = account.capabilities.require(Capability::StartConversation) {
            self.notice = Some(reason.into());
            return Task::none();
        }
        let Some(adapter) = self.adapters.get(&id).cloned() else {
            self.notice = Some("Connect this account in Settings first.".into());
            return Task::none();
        };
        let recipient = self.new_recipient.trim().to_owned();
        if recipient.is_empty() {
            return Task::none();
        }
        self.new_busy = true;
        let request = uuid::Uuid::new_v4();
        self.new_request = Some((request, id));
        let session = self.session(id);
        self.track_job(
            super::jobs::Kind::Conversation,
            async move { adapter.start_conversation(&recipient).await },
            move |result| session.wrap(Message::ConversationStarted(request, id, result)),
        )
    }

    pub(super) fn conversation_started(
        &mut self,
        request: uuid::Uuid,
        id: AccountId,
        result: Result<Conversation, AdapterError>,
    ) -> Task<Message> {
        let active = self.closing.is_none()
            && self.new_request == Some((request, id))
            && self.dialog == Some(super::Dialog::NewConversation);
        if self.new_request == Some((request, id)) {
            self.new_request = None;
            self.new_busy = false;
        }
        if !self.store.accounts.iter().any(|account| account.id == id) {
            return Task::none();
        }
        match result {
            Ok(conversation) if conversation.key.account == id => {
                let key = conversation.key.clone();
                if self.store.room(&key).is_none() {
                    self.store.conversations.push(conversation);
                }
                if active && let Some(room) = self.store.room_mut(&key) {
                    room.archived = false;
                    room.unread = 0;
                    self.editor = text_editor::Content::with_text(&room.draft);
                }

                self.changed();
                if !active {
                    return self.save();
                }
                self.selected = Some(key);
                self.show_detail = true;
                self.editing = None;
                self.new_recipient.clear();
                self.notice = None;
                self.changed();
                Task::batch([self.save(), self.focus_composer_after_dialog()])
            }
            Ok(_) if active => {
                self.notice = Some("The conversation belongs to a different account.".into());
                Task::none()
            }
            Err(error) if active => {
                self.notice = Some(error.to_string().into());
                Task::none()
            }
            _ => Task::none(),
        }
    }
}
