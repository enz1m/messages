use std::path::PathBuf;

use enzui_iced_widgets::file_drop::{DragAction, FileDrop};
use iced::Task;
use messages_core::{
    Capability, MAX_ATTACHMENT_BYTES, MAX_ATTACHMENTS, OutgoingAttachment, RoomKey,
};
use uuid::Uuid;

use super::{App, Message, lifecycle::Session};

pub struct Admission {
    pub session: Session,
    room: RoomKey,
    revision: Option<u64>,
    finish: Box<dyn FnOnce() + Send>,
}

impl App {
    pub(super) fn dropped_files(
        &mut self,
        session: Session,
        room: RoomKey,
        drop: FileDrop,
    ) -> Task<Message> {
        if drop
            .action()
            .is_some_and(|action| action != DragAction::Copy)
        {
            return Task::none();
        }
        let paths = drop.paths().to_vec();
        self.stage_drop(session, room, paths, Box::new(move || drop.finish()))
    }

    pub(super) fn stage_drop(
        &mut self,
        session: Session,
        room: RoomKey,
        paths: Vec<PathBuf>,
        finish: Box<dyn FnOnce() + Send>,
    ) -> Task<Message> {
        if !self.session_is_current(session)
            || session.account != room.account
            || self.stopping()
            || self.loading
            || self.persistence_blocked
            || self.dialog.is_some()
            || self.popup.is_some()
            || self.editing.is_some()
            || self.chooser.is_pending()
            || self.chooser_start.active.is_some()
            || self.file_busy.contains(&room)
            || !self
                .store
                .capabilities(&room)
                .supports(Capability::AttachFiles)
        {
            return Task::none();
        }
        let Some(target) = self.store.room(&room) else {
            return Task::none();
        };
        let available = MAX_ATTACHMENTS.saturating_sub(target.draft_attachments.len());
        let budget = MAX_ATTACHMENT_BYTES
            .saturating_sub(target.draft_attachments.iter().map(|file| file.bytes).sum());
        let token = Uuid::new_v4();
        self.drop_admissions.insert(
            token,
            Admission {
                session,
                room: room.clone(),
                revision: None,
                finish,
            },
        );
        self.file_busy.insert(room.clone());
        self.track_job(
            super::jobs::Kind::Stage,
            crate::platform::files::stage(
                self.file_root(&room).join("outgoing"),
                paths,
                available,
                budget,
            ),
            move |result| Message::DropStaged(token, session, room.clone(), result),
        )
    }

    pub(super) fn drop_staged(
        &mut self,
        token: Uuid,
        session: Session,
        room: RoomKey,
        result: Result<Vec<OutgoingAttachment>, String>,
    ) -> Task<Message> {
        let accepted = self.drop_admissions.contains_key(&token)
            && self.session_is_current(session)
            && self.store.room(&room).is_some()
            && result.is_ok();
        let task = self.files_staged(session, room, result);
        if accepted {
            if let Some(admission) = self.drop_admissions.get_mut(&token) {
                admission.revision = Some(self.revision);
            }
        } else {
            self.drop_admissions.remove(&token);
        }
        task
    }

    pub(super) fn settle_drops(&mut self) {
        let stale: Vec<_> = self
            .drop_admissions
            .iter()
            .filter_map(|(id, admission)| {
                (!self.session_is_current(admission.session)
                    || self.store.room(&admission.room).is_none())
                .then_some(*id)
            })
            .collect();
        for id in stale {
            self.drop_admissions.remove(&id);
        }
        let ready: Vec<_> = self
            .drop_admissions
            .iter()
            .filter_map(|(id, admission)| {
                (admission
                    .revision
                    .is_some_and(|revision| revision <= self.saved_revision)
                    && self.session_is_current(admission.session))
                .then_some(*id)
            })
            .collect();
        for id in ready {
            if let Some(admission) = self.drop_admissions.remove(&id) {
                (admission.finish)();
            }
        }
    }
}
