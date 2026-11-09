use std::collections::HashSet;

use enzui_iced_core::activation::lifecycle::{StopChoice, StopConsequence, StopRequest, WorkKind};
use iced::{Task, window};
use messages_core::{OutboxState, SendError};

use super::{App, Message};

pub struct Plan {
    pub choice: StopChoice,
    window: window::Id,
}

impl Message {
    pub(super) fn allowed_while_stopping(&self) -> bool {
        matches!(
            self,
            Self::WorkCompleted(..)
                | Self::WorkChanged
                | Self::LifecycleWindow(..)
                | Self::SessionResult(..)
                | Self::Loaded(..)
                | Self::WorkspaceLoaded(..)
                | Self::AvatarCacheSaved(..)
                | Self::Saved(..)
                | Self::ThemeSaved(..)
                | Self::NotificationsReady(..)
                | Self::NotificationWake
                | Self::ActivationFinished
                | Self::FilesDiscarded
                | Self::PresenceFinished
                | Self::PresenceReadFailed(..)
                | Self::PreviewReady(..)
                | Self::FilesStaged(..)
                | Self::DropStaged(..)
                | Self::FileFinished(..)
                | Self::ChooserWake
                | Self::ChooserWindow(..)
                | Self::ChooserNativeParent(..)
                | Self::ChooserExported(..)
                | Self::ChooserDone(..)
                | Self::CredentialLoaded(..)
                | Self::CredentialStored(..)
                | Self::CredentialDeleted(..)
                | Self::CredentialsReady(..)
                | Self::Sent(..)
                | Self::ActionDone(..)
                | Self::ClipboardWritten(..)
                | Self::ServiceLogin(..)
                | Self::ServicePrepared(..)
                | Self::ServiceRestored(..)
                | Self::ConversationStarted(..)
                | Self::ThreadStarted(..)
                | Self::HistoryLoaded(..)
                | Self::LoginLoaded(..)
                | Self::LoginSubmitted(..)
                | Self::SampleSaved(..)
                | Self::Synced(..)
                | Self::WindowFocused(..)
                | Self::WindowResized(..)
                | Self::TranscriptScrolled(..)
                | Self::ComposerMeasured(..)
                | Self::Theme(..)
                | Self::Launched
                | Self::CloseRequested(..)
                | Self::ServicesStopped(..)
                | Self::AccountErased(..)
                | Self::Tick
        )
    }
}

impl App {
    pub(crate) fn stopping(&self) -> bool {
        self.stop_mode.is_some() || self.closing.is_some()
    }

    fn stop_consequences(&self) -> Vec<StopConsequence> {
        let mut consequences: Vec<_> = self
            .jobs
            .live()
            .into_iter()
            .map(|(id, kind)| {
                StopConsequence::new(format!("job:{id}"), kind.work_kind().as_str(), kind.title())
            })
            .collect();
        if let Some(call) = self
            .calls
            .active
            .as_ref()
            .filter(|call| !call.snapshot.phase.finished())
        {
            consequences.push(StopConsequence::new(
                format!("call:{}", call.snapshot.id),
                WorkKind::Other.as_str(),
                "Active call",
            ));
        }
        for item in self
            .store
            .outbox
            .iter()
            .filter(|item| item.state == OutboxState::Queued)
        {
            consequences.push(StopConsequence::new(
                format!("queued:{}:{}", self.boot_nonce, item.request.transaction),
                "upload",
                "A queued message has not been sent",
            ));
        }
        if self.revision != self.saved_revision {
            consequences.push(StopConsequence::new(
                format!("snapshot:{}:{}", self.boot_nonce, self.revision),
                "unsaved",
                "Conversation changes and drafts have not been committed",
            ));
        }
        for token in self.notification_pending.keys() {
            consequences.push(StopConsequence::new(
                format!("notification:{token}"),
                "other",
                "Delivering a desktop notification",
            ));
        }
        consequences.sort_by(|a, b| a.key.cmp(&b.key));
        consequences
    }

    pub(crate) fn publish_lifecycle(&self) {
        if let Some(control) = &self.stop_control {
            control.publish(self.stop_consequences());
            control.publish_work(
                self.jobs
                    .live()
                    .into_iter()
                    .map(|(_, kind)| kind.work_kind())
                    .chain(
                        self.store
                            .outbox
                            .iter()
                            .filter(|item| item.state == OutboxState::Queued)
                            .map(|_| WorkKind::Upload),
                    )
                    .chain(self.notification_pending.iter().map(|_| WorkKind::Other)),
            );
        }
    }

    fn stop_review_current(&self, request: &StopRequest) -> bool {
        self.stop_control
            .as_ref()
            .is_some_and(|control| control.is_current(request))
            && self
                .stop_consequences()
                .iter()
                .map(|item| item.key.as_str())
                .collect::<HashSet<_>>()
                == request
                    .reviewed
                    .iter()
                    .map(String::as_str)
                    .collect::<HashSet<_>>()
    }

    pub(super) fn lifecycle_wake(&mut self) -> Task<Message> {
        self.publish_lifecycle();
        let request = self
            .stop_control
            .as_ref()
            .and_then(|control| control.take_stop());
        let Some(request) = request.filter(|request| self.stop_review_current(request)) else {
            return Task::none();
        };
        window::oldest().map(move |window| Message::LifecycleWindow(request.clone(), window))
    }

    pub(super) fn lifecycle_window(
        &mut self,
        request: StopRequest,
        window: Option<window::Id>,
    ) -> Task<Message> {
        self.publish_lifecycle();
        if !self.stop_review_current(&request) {
            return Task::none();
        }
        let Some(window) = window else {
            return Task::none();
        };
        self.begin_stop(request.choice, window);
        Task::none()
    }

    pub(super) fn begin_stop(&mut self, choice: StopChoice, window: window::Id) {
        self.devices.stop();
        self.retire_call();
        self.chooser_start.cancel();
        self.chooser
            .cancel(enzui_iced_widgets::file_chooser::CancelReason::Application);
        self.popup = None;
        self.context_menu.close();
        self.stop_mode = Some(Plan { choice, window });
    }

    fn cancel_stop_work(&mut self) {
        let unsent: HashSet<_> = self.dispatch_after_save.iter().copied().collect();
        self.dispatch_after_save.clear();
        self.send_admissions.clear();
        let sends: Vec<_> = self
            .store
            .outbox
            .iter()
            .filter(|item| matches!(item.state, OutboxState::Queued | OutboxState::Sending))
            .map(|item| {
                (
                    item.request.room.clone(),
                    item.request.transaction,
                    item.state.clone(),
                )
            })
            .collect();
        if !sends.is_empty() {
            for (room, transaction, state) in sends {
                let error = if state == OutboxState::Queued || unsent.contains(&transaction) {
                    SendError::Rejected("Stopped before transport dispatch.".into())
                } else {
                    SendError::Uncertain
                };
                self.store.fail_send(&room, transaction, error);
            }
            self.changed();
        }
        self.jobs.cancel_network();
        self.cancel_service();
        self.syncing.clear();
        self.history_busy.clear();
        self.managed_login_checks.clear();
        self.preview_tasks.clear();
        self.new_request = None;
        self.new_busy = false;
        self.login = None;
        self.drop_admissions.clear();
        self.notification_pending.clear();
        self.notifications = None;
        self.notification_connecting = false;
    }

    pub(super) fn advance_stop(&mut self) -> Task<Message> {
        let Some(plan) = &self.stop_mode else {
            return Task::none();
        };
        if self.closing.is_some() {
            return Task::none();
        }
        if self.persistence_blocked {
            self.stop_mode = None;
            self.notice =
                Some("Messages cannot stop safely while local storage is unavailable.".into());
            return Task::none();
        }
        let choice = plan.choice;
        let window = plan.window;
        if choice == StopChoice::CancelAndExit {
            self.cancel_stop_work();
        }
        if choice == StopChoice::FinishThenExit
            && self.dispatch_after_save.is_empty()
            && !self
                .jobs
                .live()
                .iter()
                .any(|(_, kind)| *kind == super::jobs::Kind::Send)
        {
            let orphaned: Vec<_> = self
                .store
                .outbox
                .iter()
                .filter(|item| item.state == OutboxState::Sending)
                .map(|item| (item.request.room.clone(), item.request.transaction))
                .collect();
            if !orphaned.is_empty() {
                for (room, transaction) in orphaned {
                    self.store
                        .fail_send(&room, transaction, SendError::Uncertain);
                }
                self.changed();
            }
        }
        let mut tasks = Vec::new();
        if choice == StopChoice::FinishThenExit
            && !self.saving
            && !self.theme_writer.active
            && self.theme_writer.pending.is_none()
            && self.revision == self.saved_revision
        {
            let queued: Vec<_> = self
                .store
                .outbox
                .iter()
                .filter(|item| item.state == OutboxState::Queued)
                .map(|item| item.request.transaction)
                .collect();
            for transaction in queued {
                tasks.push(self.stage_send(transaction));
            }
        }
        let save = self.save();
        let credentials = self.credential_tasks();
        self.settle_drops();
        let ready = self.jobs.live().is_empty()
            && self.dispatch_after_save.is_empty()
            && !self.loading
            && !self.saving
            && !self.theme_writer.active
            && self.theme_writer.pending.is_none()
            && self.revision == self.saved_revision
            && self.file_busy.is_empty()
            && self.file_operations.is_empty()
            && self.credential_busy.is_empty()
            && self.credential_pending.is_empty()
            && self.drop_admissions.is_empty()
            && self.notification_pending.is_empty();
        if ready {
            tasks.extend([save, credentials, self.request_close(window)]);
            return Task::batch(tasks);
        }
        tasks.extend([save, credentials]);
        Task::batch(tasks)
    }
}
