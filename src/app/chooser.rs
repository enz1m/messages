use std::time::Duration;

use enzui_iced_core::{
    blocking_task::BlockingTaskSettlement,
    portal::{CancelReason, cancel::PortalCancel},
};
use enzui_iced_widgets::{
    export_pool::ExportPoolOwner,
    file_chooser::{ChooserOptions, ChooserParent, PortalOperation, PortalOutcome},
    portal_parent::{ParentExport, export_parent, export_parent_task},
};
use enzui_parent_lease::Handoff;
use enzui_wayland::ParentAdoption;
use iced::{Task, window};
use uuid::Uuid;

use super::{App, Message, files::Purpose};

pub type ExportReply = Handoff<BlockingTaskSettlement<ParentAdoption>>;

#[derive(Clone, Debug)]
pub struct Ticket {
    pub token: Uuid,
    pub purpose: Purpose,
}

pub struct Pending {
    token: Uuid,
    tag: Purpose,
    operation: PortalOperation,
    options: ChooserOptions,
    cancel: PortalCancel,
}

#[derive(Default)]
pub struct Controller {
    pub pending: Option<Pending>,
    pub active: Option<Uuid>,
    pool: Option<ExportPoolOwner>,
    generation: u64,
}

impl Controller {
    pub fn cancel(&mut self) {
        self.active = None;
        if let Some(pending) = self.pending.take() {
            pending.cancel.cancel(CancelReason::Application);
        }
    }
}

impl Drop for Controller {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl App {
    pub(super) fn start_chooser(
        &mut self,
        tag: Purpose,
        operation: PortalOperation,
        options: ChooserOptions,
    ) -> Task<Message> {
        if self.chooser.is_pending() || self.chooser_start.pending.is_some() || self.stopping() {
            return Task::none();
        }
        let token = Uuid::new_v4();
        self.chooser_start.active = Some(token);
        self.chooser_start.pending = Some(Pending {
            token,
            tag,
            operation,
            options,
            cancel: PortalCancel::new(),
        });
        window::oldest().map(move |window| Message::ChooserWindow(token, window))
    }

    fn chooser_token_current(&mut self, token: Uuid) -> bool {
        let current = !self.stopping()
            && self.chooser_start.active == Some(token)
            && self.chooser_start.pending.as_ref().is_some_and(|pending| {
                pending.token == token && self.session_is_current(pending.tag.session())
            });
        if !current
            && self
                .chooser_start
                .pending
                .as_ref()
                .is_some_and(|pending| pending.token == token)
        {
            self.chooser_start.cancel();
        }
        current
    }

    pub(super) fn chooser_window(
        &mut self,
        token: Uuid,
        window: Option<window::Id>,
    ) -> Task<Message> {
        if !self.chooser_token_current(token) {
            return Task::none();
        }
        let Some(window) = window else {
            return self.chooser_parent_failed(token);
        };
        window::run(window, |native| {
            native
                .window_handle()
                .ok()
                .map(|handle| export_parent(&handle.as_raw()))
        })
        .map(move |parent| Message::ChooserNativeParent(token, window, parent))
    }

    pub(super) fn chooser_native_parent(
        &mut self,
        token: Uuid,
        window: window::Id,
        parent: Option<ParentExport>,
    ) -> Task<Message> {
        if !self.chooser_token_current(token) {
            return Task::none();
        }
        match parent {
            Some(ParentExport::Ready(handle)) => {
                self.chooser_parent_ready(token, window, ChooserParent::X11 { window, handle })
            }
            Some(ParentExport::NeedsWaylandExport) => {
                let owner = self.chooser_start.pool.get_or_insert_with(|| {
                    ExportPoolOwner::start().unwrap_or_else(|_| ExportPoolOwner::unavailable())
                });
                let pool = owner.dispatcher();
                self.chooser_start.generation = self.chooser_start.generation.wrapping_add(1);
                let pending = self
                    .chooser_start
                    .pending
                    .as_ref()
                    .expect("current chooser request");
                export_parent_task(
                    &pool,
                    self.chooser_start.generation,
                    window,
                    pending.cancel.clone(),
                    Duration::from_secs(4),
                )
                .map(move |reply| Message::ChooserExported(token, window, reply))
            }
            _ => self.chooser_parent_failed(token),
        }
    }

    pub(super) fn chooser_exported(
        &mut self,
        token: Uuid,
        window: window::Id,
        reply: ExportReply,
    ) -> Task<Message> {
        let Some(reply) = reply.take() else {
            return Task::none();
        };
        if !self.chooser_token_current(token) {
            return Task::none();
        }
        match reply.into_result() {
            Ok(ParentAdoption::Parented { export, .. }) => {
                self.chooser_parent_ready(token, window, ChooserParent::Wayland { window, export })
            }
            _ => self.chooser_parent_failed(token),
        }
    }

    fn chooser_parent_ready(
        &mut self,
        token: Uuid,
        window: window::Id,
        parent: ChooserParent,
    ) -> Task<Message> {
        if !self.chooser_token_current(token) {
            return Task::none();
        }
        let Some(pending) = self.chooser_start.pending.take() else {
            return Task::none();
        };
        // Messages owns one window; close admission and the retained export gate its liveness.
        let live = self.closing.is_none();
        if let Err(outcome) = self.chooser.start_parented(
            Ticket {
                token,
                purpose: pending.tag.clone(),
            },
            pending.operation,
            pending.options,
            parent,
            |candidate| live && candidate == window,
        ) {
            return self.chooser_done(
                Ticket {
                    token,
                    purpose: pending.tag,
                },
                outcome,
            );
        }
        Task::none()
    }

    fn chooser_parent_failed(&mut self, token: Uuid) -> Task<Message> {
        if !self.chooser_token_current(token) {
            return Task::none();
        }
        let Some(pending) = self.chooser_start.pending.take() else {
            return Task::none();
        };
        self.chooser_done(
            Ticket {
                token,
                purpose: pending.tag,
            },
            PortalOutcome::ParentUnavailable,
        )
    }
}
