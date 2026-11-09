//! Erasing a removed account's helper storage and attachment files.
//!
//! Removing an account commits a durable tombstone (`Snapshot::account_purges`)
//! in the same snapshot that drops the account. Erasure then waits for a
//! stop/settlement barrier before it touches `services/<account>` (SDK
//! databases, Signal configuration and keys) or `files/<account>` (staged and
//! opened attachments):
//!
//! * the snapshot that no longer names the account is committed, so no
//!   outbox entry can still claim custody of a staged file;
//! * the account's helper process has been stopped and reaped;
//! * no account setup, attachment staging, file or preview work is live, so
//!   a stale setup or late file write cannot race the erasure.
//!
//! The tombstone survives an in-session erasure and is cleared only by the
//! sweep at the next start, which runs before any helper or file work for the
//! session exists. A missing settings row is therefore never taken as proof
//! that an account's files are gone.

use iced::Task;
use messages_core::AccountId;

use super::{App, Message, jobs::Kind};

impl App {
    /// Record a removed account for erasure once `revision` is committed.
    pub(super) fn schedule_account_erasure(&mut self, account: AccountId, revision: u64) {
        if !self.account_purges.contains(&account) {
            self.account_purges.push(account);
        }
        self.erase_after.retain(|(_, pending)| *pending != account);
        self.erase_after.push((revision, account));
    }

    fn account_root(&self) -> std::path::PathBuf {
        self.storage_path
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .to_owned()
    }

    /// Whether no work that may write into an account's directories is live.
    fn erasure_barrier_clear(&self) -> bool {
        !self.jobs.live().iter().any(|(_, kind)| {
            matches!(
                kind,
                Kind::AccountSetup | Kind::Stage | Kind::File | Kind::Preview
            )
        })
    }

    /// Start every in-session erasure whose removal is committed and whose
    /// barrier is clear. Called after each save and each settled job.
    pub(super) fn drive_account_erasure(&mut self) -> Task<Message> {
        if self.loading || self.persistence_blocked || self.erase_after.is_empty() {
            return Task::none();
        }
        let ready: Vec<AccountId> = self
            .erase_after
            .iter()
            .filter(|(revision, account)| {
                *revision <= self.saved_revision && !self.erasing.contains(account)
            })
            .map(|(_, account)| *account)
            .collect();
        if ready.is_empty() || !self.erasure_barrier_clear() {
            return Task::none();
        }
        let root = self.account_root();
        Task::batch(ready.into_iter().map(|account| {
            self.erase_after.retain(|(_, pending)| *pending != account);
            self.erasing.insert(account);
            let host = self.erase_hosts.remove(&account);
            let root = root.clone();
            self.track_job(
                Kind::AccountErase,
                async move {
                    if let Some(host) = host {
                        host.stop().await;
                    }
                    crate::platform::services::erase_account_storage(root, account).await
                },
                move |result| Message::AccountErased(account, false, result),
            )
        }))
    }

    /// Erase every tombstoned account left by a previous session.
    ///
    /// Runs right after loading: no helper and no file work of this process
    /// exists for a removed account, so the process boundary is the barrier.
    /// A tombstone naming an account that is still saved is dropped without
    /// erasing anything: live account data is never deleted.
    pub(super) fn sweep_account_erasures(&mut self) -> Task<Message> {
        let before = self.account_purges.len();
        let accounts = &self.store.accounts;
        self.account_purges
            .retain(|purge| !accounts.iter().any(|account| account.id == *purge));
        if self.account_purges.len() != before {
            self.changed();
        }
        let root = self.account_root();
        let pending: Vec<_> = self
            .account_purges
            .iter()
            .copied()
            .filter(|account| self.erasing.insert(*account))
            .collect();
        Task::batch(pending.into_iter().map(|account| {
            self.track_job(
                Kind::AccountErase,
                crate::platform::services::erase_account_storage(root.clone(), account),
                move |result| Message::AccountErased(account, true, result),
            )
        }))
    }

    pub(super) fn account_erased(
        &mut self,
        account: AccountId,
        sweep: bool,
        result: Result<(), String>,
    ) -> Task<Message> {
        self.erasing.remove(&account);
        match result {
            Ok(()) if sweep => {
                self.account_purges.retain(|purge| *purge != account);
                self.changed();
                self.save()
            }
            // The tombstone stays until the next start's sweep confirms nothing
            // reappeared after the in-session barrier.
            Ok(()) => Task::none(),
            Err(error) => {
                self.notice = Some(
                    format!("{error} Messages will try again the next time it starts.").into(),
                );
                Task::none()
            }
        }
    }
}
