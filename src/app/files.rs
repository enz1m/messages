use std::path::PathBuf;

use enzui_iced_widgets::file_chooser::{
    ChooserOptions, FileResult, PortalOperation, PortalOutcome,
};
use iced::Task;
use messages_core::{
    Attachment, Capability, MAX_ATTACHMENT_BYTES, MAX_ATTACHMENTS, OutgoingAttachment, RoomKey,
};

use super::{App, Message, lifecycle::Session};

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum Purpose {
    Attach(Session, RoomKey),
    Save(Session, RoomKey, String, String),
    Open(Session, RoomKey, String, String),
}

impl Purpose {
    pub fn session(&self) -> Session {
        match self {
            Self::Attach(session, _) | Self::Save(session, ..) | Self::Open(session, ..) => {
                *session
            }
        }
    }
}

impl App {
    pub fn attachment_busy(&self, key: &RoomKey, message: &str, file: &str) -> bool {
        self.file_operations.iter().any(|purpose| match purpose {
            Purpose::Open(_, room, id, attachment) | Purpose::Save(_, room, id, attachment) => {
                room == key && id == message && attachment == file
            }
            Purpose::Attach(..) => false,
        })
    }

    pub(super) fn file_root(&self, key: &RoomKey) -> PathBuf {
        self.storage_path
            .with_file_name("files")
            .join(key.account.to_string())
    }

    pub(super) fn attach_files(&mut self) -> Task<Message> {
        let Some(key) = self.selected.clone() else {
            return Task::none();
        };
        if self.closing.is_some()
            || self.loading
            || self.persistence_blocked
            || self.editing.is_some()
            || self.file_busy.contains(&key)
        {
            return Task::none();
        }
        if let Err(reason) = self
            .store
            .capabilities(&key)
            .require(Capability::AttachFiles)
        {
            self.notice = Some(reason.into());
            return Task::none();
        }
        self.popup = None;
        let purpose = Purpose::Attach(self.session(key.account), key);
        let options = ChooserOptions::new().title("Attach files");
        self.start_chooser(purpose, PortalOperation::OpenMultiple, options)
    }

    pub(super) fn chooser_done(
        &mut self,
        ticket: super::chooser::Ticket,
        outcome: PortalOutcome<FileResult>,
    ) -> Task<Message> {
        if self.chooser_start.active != Some(ticket.token) {
            return Task::none();
        }
        self.chooser_start.active = None;
        let purpose = ticket.purpose;
        if self.closing.is_some() || !self.session_is_current(purpose.session()) {
            return Task::none();
        }
        let result = match outcome {
            PortalOutcome::Completed(result) => result,
            PortalOutcome::Cancelled(_) => return Task::none(),
            outcome => {
                self.notice =
                    enzui_iced_widgets::file_chooser::failure_text(&outcome).map(Into::into);
                return Task::none();
            }
        };
        let paths: Result<Vec<_>, _> = result.uris.iter().map(|uri| uri.to_path()).collect();
        let Ok(paths) = paths else {
            self.notice = Some("Choose files from this computer.".into());
            return Task::none();
        };
        match purpose {
            Purpose::Attach(session, key) => {
                let Some(room) = self.store.room(&key) else {
                    return Task::none();
                };
                if self
                    .store
                    .capabilities(&key)
                    .require(Capability::AttachFiles)
                    .is_err()
                    || self.file_busy.contains(&key)
                {
                    return Task::none();
                }
                let available = MAX_ATTACHMENTS.saturating_sub(room.draft_attachments.len());
                let budget = MAX_ATTACHMENT_BYTES
                    .saturating_sub(room.draft_attachments.iter().map(|file| file.bytes).sum());
                let root = self.file_root(&key).join("outgoing");
                self.file_busy.insert(key.clone());
                self.track_job(
                    super::jobs::Kind::Stage,
                    crate::platform::files::stage(root, paths, available, budget),
                    move |result| Message::FilesStaged(session, key.clone(), result),
                )
            }
            Purpose::Save(_, key, message, file) => {
                let Some(path) = paths.first().cloned() else {
                    return Task::none();
                };
                self.transfer(key, message, file, Some(path))
            }
            Purpose::Open(..) => Task::none(),
        }
    }

    pub(super) fn files_staged(
        &mut self,
        session: Session,
        key: RoomKey,
        result: Result<Vec<OutgoingAttachment>, String>,
    ) -> Task<Message> {
        if !self.session_is_current(session)
            || key.account != session.account
            || self.store.room(&key).is_none()
        {
            return if let Ok(files) = result {
                self.track_job(
                    super::jobs::Kind::File,
                    crate::platform::files::discard(self.file_root(&key).join("outgoing"), files),
                    |_| Message::FilesDiscarded,
                )
            } else {
                Task::none()
            };
        }
        self.file_busy.remove(&key);
        match result {
            Ok(files) => {
                if let Some(room) = self.store.room_mut(&key) {
                    room.draft_attachments.extend(files);
                }
                self.changed();
                return Task::batch([self.save(), self.load_previews()]);
            }
            Err(error) => self.notice = Some(error.into()),
        }
        Task::none()
    }

    pub(super) fn remove_attachment(&mut self, id: uuid::Uuid) -> Task<Message> {
        let Some(key) = self.selected.clone() else {
            return Task::none();
        };
        let Some(room) = self.store.room_mut(&key) else {
            return Task::none();
        };
        if let Some(index) = room.draft_attachments.iter().position(|file| file.id == id) {
            let file = room.draft_attachments.remove(index);
            self.changed();
            self.file_reclaims.push((self.revision, key, file));
            return self.save();
        }
        Task::none()
    }

    pub(super) fn reclaim_files(&mut self) -> Task<Message> {
        let (ready, pending): (Vec<_>, Vec<_>) = std::mem::take(&mut self.file_reclaims)
            .into_iter()
            .partition(|(revision, _, _)| *revision <= self.saved_revision);
        self.file_reclaims = pending;
        Task::batch(ready.into_iter().map(|(_, key, file)| {
            self.track_job(
                super::jobs::Kind::File,
                crate::platform::files::discard(self.file_root(&key).join("outgoing"), vec![file]),
                |_| Message::FilesDiscarded,
            )
        }))
    }

    fn attached_file(&self, key: &RoomKey, message: &str, id: &str) -> Option<Attachment> {
        self.store
            .room(key)?
            .messages
            .iter()
            .find(|item| item.id == message && !item.deleted)?
            .attachments
            .iter()
            .find(|file| file.id == id)
            .cloned()
    }

    pub(super) fn save_attachment(
        &mut self,
        key: RoomKey,
        message: String,
        id: String,
    ) -> Task<Message> {
        let Some(file) = self.attached_file(&key, &message, &id) else {
            return Task::none();
        };
        let purpose = Purpose::Save(self.session(key.account), key, message, id);
        let options = ChooserOptions::new()
            .title("Save attachment")
            .current_name(crate::platform::files::safe_name(&file.name));
        self.start_chooser(purpose, PortalOperation::SaveFile, options)
    }

    pub(super) fn open_attachment(
        &mut self,
        key: RoomKey,
        message: String,
        id: String,
    ) -> Task<Message> {
        self.transfer(key, message, id, None)
    }

    fn transfer(
        &mut self,
        key: RoomKey,
        message: String,
        id: String,
        destination: Option<PathBuf>,
    ) -> Task<Message> {
        let Some(file) = self.attached_file(&key, &message, &id) else {
            return Task::none();
        };
        if self.attachment_busy(&key, &message, &id) {
            return Task::none();
        }
        let session = self.session(key.account);
        let purpose = if destination.is_some() {
            Purpose::Save(session, key.clone(), message, id)
        } else {
            Purpose::Open(session, key.clone(), message, id)
        };
        let adapter = self.adapters.get(&key.account).cloned();
        let root = self.file_root(&key);
        let files_root = self.storage_path.with_file_name("files");
        let account = key.account;
        self.file_operations.insert(purpose.clone());
        let completion = purpose.clone();
        let (task, handle) = self
            .track_job(
                super::jobs::Kind::File,
                async move {
                    let bytes = if file.id.starts_with("local:") {
                        crate::platform::files::read_private(
                            root.join("outgoing"),
                            file.remote_url
                                .as_deref()
                                .ok_or("The local attachment is unavailable.")?,
                        )
                        .await?
                    } else {
                        adapter
                            .ok_or("Reconnect this account to download the file.")?
                            .attachment(&key, &file)
                            .await
                            .map_err(|error| error.to_string())?
                    };
                    if let Some(destination) = destination {
                        crate::platform::files::publish(destination, bytes).await?;
                    } else {
                        let path =
                            crate::platform::files::cache(files_root, account, file.name, bytes)
                                .await?;
                        crate::platform::open::file(path).await?;
                    }
                    Ok(())
                },
                move |result| Message::FileFinished(completion.clone(), result),
            )
            .abortable();
        self.file_tasks.insert(purpose, handle.abort_on_drop());
        task
    }
}
