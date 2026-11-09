use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

use enzui_iced_core::{activation::lifecycle::WorkKind, portal::CompletionSignal};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    Loading,
    Sync,
    Send,
    RemoteAction,
    Conversation,
    Presence,
    Login,
    AuthenticationCleanup,
    AccountSetup,
    AccountErase,
    History,
    Preview,
    Notifications,
    Persistence,
    Credentials,
    Stage,
    File,
    Shutdown,
    Call,
    MediaCheck,
}

impl Kind {
    pub fn work_kind(self) -> WorkKind {
        match self {
            Self::Sync | Self::History => WorkKind::Sync,
            Self::Send => WorkKind::Upload,
            Self::Stage => WorkKind::Copy,
            Self::File | Self::Preview => WorkKind::Download,
            _ => WorkKind::Other,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Sync => "Synchronizing conversations",
            Self::Send => "Sending a message",
            Self::RemoteAction => "Updating a message",
            Self::Conversation => "Starting a conversation",
            Self::Presence => "Sharing conversation presence",
            Self::Loading => "Loading saved conversations",
            Self::Login => "Checking account sign-in",
            Self::AuthenticationCleanup => "Retiring browser sign-in",
            Self::AccountSetup => "Starting an account connection",
            Self::AccountErase => "Erasing a removed account's data",
            Self::History => "Loading conversation history",
            Self::Preview => "Loading an attachment preview",
            Self::Notifications => "Connecting desktop notifications",
            Self::Persistence => "Saving conversations and drafts",
            Self::Credentials => "Settling protected account credentials",
            Self::Stage => "Preparing attachment custody",
            Self::File => "Completing an attachment operation",
            Self::Shutdown => "Stopping account helpers",
            Self::Call => "Managing a call",
            Self::MediaCheck => "Checking media devices",
        }
    }

    fn cancelable(self) -> bool {
        !matches!(
            self,
            Self::Loading
                | Self::Persistence
                | Self::Credentials
                | Self::AuthenticationCleanup
                | Self::AccountErase
                | Self::Stage
                | Self::File
                | Self::Shutdown
        )
    }
}

struct Job {
    kind: Kind,
    handle: Option<iced::task::Handle>,
}

#[derive(Default)]
struct Inner {
    jobs: Mutex<HashMap<Uuid, Job>>,
    wake: CompletionSignal,
}

#[derive(Clone, Default)]
pub struct Registry(Arc<Inner>);

impl Registry {
    pub fn begin(&self, kind: Kind) -> Completion {
        let id = Uuid::new_v4();
        self.0
            .jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id, Job { kind, handle: None });
        self.0.wake.notify();
        Completion(Arc::new(CompletionInner {
            id,
            registry: Arc::downgrade(&self.0),
            finished: AtomicBool::new(false),
        }))
    }

    pub fn attach(&self, completion: &Completion, handle: iced::task::Handle) {
        let mut handle = Some(handle.abort_on_drop());
        let mut jobs = self
            .0
            .jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(job) = jobs.get_mut(&completion.0.id) {
            job.handle = handle.take();
        }
        drop(jobs);
        drop(handle);
    }

    pub fn live(&self) -> Vec<(Uuid, Kind)> {
        let mut jobs: Vec<_> = self
            .0
            .jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|(id, job)| (*id, job.kind))
            .collect();
        jobs.sort_by_key(|(id, _)| *id);
        jobs
    }

    pub fn cancel_network(&self) {
        let removed = {
            let mut jobs = self
                .0
                .jobs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let ids: Vec<_> = jobs
                .iter()
                .filter_map(|(id, job)| job.kind.cancelable().then_some(*id))
                .collect();
            ids.into_iter()
                .filter_map(|id| jobs.remove(&id))
                .collect::<Vec<_>>()
        };
        // Abort outside the lock: dropping a future retires its completion guard.
        let changed = !removed.is_empty();
        drop(removed);
        if changed {
            self.0.wake.notify();
        }
    }

    pub fn signal(&self) -> CompletionSignal {
        self.0.wake.clone()
    }
}

struct CompletionInner {
    id: Uuid,
    registry: Weak<Inner>,
    finished: AtomicBool,
}

impl CompletionInner {
    fn finish(&self) {
        if self.finished.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some(registry) = self.registry.upgrade() {
            let removed = registry
                .jobs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&self.id);
            drop(removed);
            registry.wake.notify();
        }
    }
}

impl Drop for CompletionInner {
    fn drop(&mut self) {
        self.finish();
    }
}

#[derive(Clone)]
pub struct Completion(Arc<CompletionInner>);

impl Completion {
    pub fn finish(&self) {
        self.0.finish();
    }
}

impl std::fmt::Debug for Completion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("Completion")
            .field(&self.0.id)
            .finish()
    }
}

impl super::App {
    pub(super) fn track_job<T: Send + 'static>(
        &self,
        kind: Kind,
        future: impl std::future::Future<Output = T> + Send + 'static,
        result: impl Fn(T) -> super::Message + Send + 'static,
    ) -> iced::Task<super::Message> {
        let completion = self.jobs.begin(kind);
        self.track_completion(completion, future, result)
    }

    pub(super) fn track_completion<T: Send + 'static>(
        &self,
        completion: Completion,
        future: impl std::future::Future<Output = T> + Send + 'static,
        result: impl Fn(T) -> super::Message + Send + 'static,
    ) -> iced::Task<super::Message> {
        let admitted = completion.clone();
        let (task, handle) = iced::Task::perform(
            async move { (completion, future.await) },
            move |(completion, value)| {
                super::Message::WorkCompleted(completion, Box::new(result(value)))
            },
        )
        .abortable();
        self.jobs.attach(&admitted, handle);
        task
    }
}
