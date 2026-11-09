use super::{App, Dialog, Message, jobs, lifecycle};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use iced::Task;
use messages_core::{AccountId, RoomKey};
use messages_protocols::{
    Adapter, AdapterError,
    calls::{CallCommand, CallInvite, CallKind, CallPhase, CallSnapshot},
};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone, Debug)]
pub enum Event {
    Start(RoomKey, CallKind, lifecycle::Session),
    Accept(Uuid, lifecycle::Session),
    Mute(Uuid, lifecycle::Session, bool),
    End(Uuid, lifecycle::Session),
    Camera(Uuid, lifecycle::Session, bool),
    Tick,
    Updated(
        lifecycle::Session,
        Uuid,
        Uuid,
        Result<CallSnapshot, AdapterError>,
    ),
}

pub struct Active {
    pub room: RoomKey,
    pub session: lifecycle::Session,
    pub snapshot: CallSnapshot,
    pub preview: Option<iced::widget::image::Handle>,
    pub error: Option<String>,
    adapter: Arc<dyn Adapter>,
    pub busy: bool,
    pub polling: bool,
    request: Uuid,
    job: Option<iced::task::Handle>,
    last_poll: std::time::Instant,
}

#[derive(Default)]
pub struct State {
    pub active: Option<Active>,
    pub present: bool,
    cleanup: Vec<(Arc<dyn Adapter>, RoomKey, Uuid)>,
}

impl App {
    pub(super) fn retire_call(&mut self) {
        self.calls.present = false;
        if let Some(call) = self.calls.active.take() {
            self.calls
                .cleanup
                .push((call.adapter, call.room, call.snapshot.id));
        }
    }

    pub(super) fn call_cleanup(&mut self) -> Task<Message> {
        let window = self.closing;
        Task::batch(std::mem::take(&mut self.calls.cleanup).into_iter().map(
            |(adapter, room, id)| {
                self.track_job(
                    jobs::Kind::Shutdown,
                    async move {
                        let _ = tokio::time::timeout(
                            std::time::Duration::from_secs(6),
                            adapter.call_control(&room, CallCommand::End { id }),
                        )
                        .await;
                    },
                    move |_| {
                        window
                            .map(Message::CloseRequested)
                            .unwrap_or(Message::ActivationFinished)
                    },
                )
            },
        ))
    }

    pub(super) fn admit_call_invites(&mut self, account: AccountId, invites: Vec<CallInvite>) {
        if self.stopping()
            || self
                .calls
                .active
                .as_ref()
                .is_some_and(|call| !call.snapshot.phase.finished())
        {
            return;
        }
        for invite in invites.into_iter().take(1) {
            if invite.room.account != account
                || invite.call.phase != CallPhase::Incoming
                || invite.call.id.is_nil()
                || !self
                    .store
                    .room(&invite.room)
                    .is_some_and(|room| room.kind == messages_core::RoomKind::Direct)
                || !self
                    .store
                    .capabilities(&invite.room)
                    .supports(invite.call.kind.capability())
            {
                continue;
            }
            let Some(adapter) = self.adapters.get(&account).cloned() else {
                continue;
            };
            self.retire_call();
            self.calls.active = Some(Active {
                session: self.session(account),
                room: invite.room,
                snapshot: invite.call,
                preview: None,
                error: None,
                adapter,
                busy: false,
                polling: false,
                request: Uuid::new_v4(),
                job: None,
                last_poll: std::time::Instant::now(),
            });
            self.calls.present = true;
        }
    }

    pub(super) fn call_event(&mut self, event: Event) -> Task<Message> {
        if self.stopping() {
            return Task::none();
        }
        if let Event::Start(room, kind, ticket) = event {
            if !self.session_is_current(ticket) || ticket.account != room.account {
                return Task::none();
            }
            if self
                .calls
                .active
                .as_ref()
                .is_some_and(|call| !call.snapshot.phase.finished())
            {
                return self.open_dialog(Dialog::Call);
            }
            if let Err(reason) = self.store.capabilities(&room).require(kind.capability()) {
                self.notice = Some(reason.into());
                return Task::none();
            }
            let Some(adapter) = self.adapters.get(&room.account).cloned() else {
                return Task::none();
            };
            if self.selected.as_ref() != Some(&room) || self.demo {
                return Task::none();
            }
            let devices = self.preferences.media_devices.clone();
            if let Err(reason) =
                messages_media::validate_selection(&devices, kind == CallKind::Video)
            {
                self.notice = Some(reason.into());
                self.settings_page = "devices".into();
                self.settings_drilled_in = true;
                let open = self.open_dialog(Dialog::Settings);
                return Task::batch([open, self.device_event(super::devices::Event::Refresh)]);
            }
            self.retire_call();
            let id = Uuid::new_v4();
            let session = self.session(room.account);
            self.calls.active = Some(Active {
                room: room.clone(),
                session,
                snapshot: CallSnapshot {
                    id,
                    kind,
                    encryption: self
                        .store
                        .room(&room)
                        .map(|r| r.encryption)
                        .unwrap_or(messages_core::Encryption::Unavailable),
                    phase: CallPhase::Calling,
                    muted: false,
                    camera_on: kind == CallKind::Video,
                    preview_png: None,
                },
                adapter,
                preview: None,
                error: None,
                busy: false,
                polling: false,
                request: Uuid::new_v4(),
                job: None,
                last_poll: std::time::Instant::now(),
            });
            self.calls.present = true;
            return self.call_request(CallCommand::Start {
                id,
                kind,
                devices,
                expires_at: chrono::Utc::now().timestamp() + 30,
            });
        }
        if let Event::Updated(session, id, request, result) = event {
            if !self.session_is_current(session) {
                return Task::none();
            }
            let Some(call) = self.calls.active.as_mut().filter(|call| {
                call.session == session && call.snapshot.id == id && call.request == request
            }) else {
                return Task::none();
            };
            call.job.take();
            call.busy = false;
            call.polling = false;
            match result {
                Ok(mut snapshot) if snapshot.id == id && snapshot.kind == call.snapshot.kind => {
                    if let Some(encoded) = snapshot
                        .preview_png
                        .take()
                        .filter(|s| s.len() <= 6 * 1024 * 1024)
                        && let Ok(bytes) = STANDARD.decode(encoded)
                        && bytes.len() <= 4 * 1024 * 1024
                    {
                        call.preview = Some(iced::widget::image::Handle::from_bytes(bytes));
                    }
                    if snapshot.phase.finished() {
                        call.preview = None;
                    }
                    call.snapshot = snapshot;
                }
                _ => {
                    call.snapshot.phase = CallPhase::Failed;
                    call.preview = None;
                    call.error = Some(
                        "The call could not be completed. Check the connection and devices.".into(),
                    );
                    self.calls
                        .cleanup
                        .push((call.adapter.clone(), call.room.clone(), id));
                }
            }
            return Task::none();
        }
        let Some(call) = self.calls.active.as_ref() else {
            return Task::none();
        };
        if !self.session_is_current(call.session) {
            self.retire_call();
            return Task::none();
        }
        let id = call.snapshot.id;
        let target = match &event {
            Event::Accept(id, session)
            | Event::End(id, session)
            | Event::Mute(id, session, _)
            | Event::Camera(id, session, _) => Some((*id, *session)),
            _ => None,
        };
        if target.is_some_and(|(id, session)| id != call.snapshot.id || session != call.session) {
            return Task::none();
        }
        match event {
            Event::End(..) => {
                self.retire_call();
                self.close_dialog()
            }
            Event::Accept(..)
                if call.snapshot.phase == CallPhase::Incoming && (!call.busy || call.polling) =>
            {
                let devices = self.preferences.media_devices.clone();
                if let Err(reason) = messages_media::validate_selection(
                    &devices,
                    call.snapshot.kind == CallKind::Video,
                ) {
                    self.notice = Some(reason.into());
                    return Task::none();
                }
                self.call_request(CallCommand::Accept { id, devices })
            }
            Event::Mute(_, _, muted)
                if (!call.busy || call.polling)
                    && matches!(
                        call.snapshot.phase,
                        CallPhase::Connected | CallPhase::Connecting | CallPhase::Calling
                    ) =>
            {
                self.call_request(CallCommand::Mute { id, muted })
            }
            Event::Camera(_, _, enabled)
                if (!call.busy || call.polling)
                    && call.snapshot.kind == CallKind::Video
                    && !call.snapshot.phase.finished() =>
            {
                self.call_request(CallCommand::Camera { id, enabled })
            }
            Event::Tick
                if !call.busy
                    && !call.snapshot.phase.finished()
                    && call.last_poll.elapsed().as_millis() >= 100 =>
            {
                self.call_request(CallCommand::Poll { id })
            }
            _ => Task::none(),
        }
    }

    fn call_request(&mut self, command: CallCommand) -> Task<Message> {
        let Some(call) = self.calls.active.as_mut() else {
            return Task::none();
        };
        let adapter = call.adapter.clone();
        let room = call.room.clone();
        let session = call.session;
        let id = call.snapshot.id;
        let request = Uuid::new_v4();
        call.request = request;
        call.polling = matches!(command, CallCommand::Poll { .. });
        call.busy = true;
        call.last_poll = std::time::Instant::now();
        let task = self.track_job(
            jobs::Kind::Call,
            async move { adapter.call_control(&room, command).await },
            move |result| Message::Call(Event::Updated(session, id, request, result)),
        );
        let (task, handle) = task.abortable();
        if let Some(call) = self.calls.active.as_mut() {
            call.job = Some(handle.abort_on_drop());
        }
        task
    }
}
