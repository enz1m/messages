use super::{App, Dialog, Message};
use iced::Task;
use messages_core::{Capability, RoomKey};

impl App {
    pub(super) fn start_thread(&mut self, parent: RoomKey) -> Task<Message> {
        if self.new_busy || self.loading || self.persistence_blocked || self.closing.is_some() {
            return Task::none();
        }
        if let Err(reason) = self
            .store
            .capabilities(&parent)
            .require(Capability::Threads)
        {
            self.notice = Some(reason.into());
            return Task::none();
        }
        let Some(adapter) = self.adapters.get(&parent.account).cloned() else {
            self.notice = Some("Reconnect this account first.".into());
            return Task::none();
        };
        let name = self.thread_name.trim().to_owned();
        if name.is_empty() {
            return Task::none();
        }
        let session = self.session(parent.account);
        let requested = parent.clone();
        self.new_busy = true;
        let request = uuid::Uuid::new_v4();
        self.new_request = Some((request, parent.account));
        self.track_job(
            super::jobs::Kind::Conversation,
            async move { adapter.start_thread(&requested, &name).await },
            move |result| session.wrap(Message::ThreadStarted(request, parent.clone(), result)),
        )
    }

    pub(super) fn thread_started(
        &mut self,
        request: uuid::Uuid,
        parent: RoomKey,
        result: Result<messages_core::Conversation, messages_protocols::AdapterError>,
    ) -> Task<Message> {
        let active = self.closing.is_none()
            && self.new_request == Some((request, parent.account))
            && self.dialog == Some(Dialog::NewThread(parent.clone()));
        if self.new_request == Some((request, parent.account)) {
            self.new_request = None;
            self.new_busy = false;
        }
        match result {
            Ok(room)
                if room.key.account == parent.account
                    && room.parent_room.as_ref() == Some(&parent.room) =>
            {
                let key = room.key.clone();
                if self.store.room(&key).is_none() {
                    self.store.conversations.push(room);
                }
                self.collapsed_rooms.remove(&parent);
                self.changed();
                if active {
                    return self
                        .update(Message::Select(key))
                        .chain(self.close_dialog())
                        .chain(self.save());
                }
                self.save()
            }
            Ok(_) if active => {
                self.notice = Some("This thread belongs to another conversation.".into());
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
