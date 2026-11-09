use super::{App, lifecycle::Session, notice::Notice};
use messages_core::{AccountId, ConnectionState};
use messages_protocols::{AdapterError, SyncBatch};
use std::time::{Duration, Instant};

pub struct Retry {
    first: Instant,
    attempts: u32,
    pub(super) due: Instant,
}

pub fn temporary(error: &AdapterError) -> bool {
    matches!(
        error,
        AdapterError::Network | AdapterError::Http(408 | 500..=599) | AdapterError::RateLimited(_)
    )
}

impl App {
    pub(super) fn sync_due(&self, account: AccountId) -> bool {
        self.sync_retry
            .get(&account)
            .is_none_or(|r| r.due <= Instant::now())
    }
    pub(super) fn sync_finished(
        &mut self,
        account: AccountId,
        result: Result<SyncBatch, AdapterError>,
    ) {
        self.syncing.remove(&account);
        let session = self.session(account);
        match result {
            Ok(batch) => {
                self.sync_retry.remove(&account);
                if self.notice.as_ref().is_some_and(|n| n.sync_owner(session)) {
                    self.notice = None;
                }
                self.apply_sync(account, batch);
            }
            Err(error) if temporary(&error) => self.defer_sync(session, &error),
            Err(error) => {
                self.sync_retry.remove(&account);
                if let Some(entry) = self.store.accounts.iter_mut().find(|a| a.id == account) {
                    entry.connection = if matches!(error, AdapterError::Authentication) {
                        ConnectionState::NeedsAuthentication(error.to_string())
                    } else {
                        ConnectionState::Failed(error.to_string())
                    };
                    self.notice = Some(format!("{}: {error}", entry.label).into());
                }
            }
        }
    }
    pub(super) fn defer_sync(&mut self, session: Session, error: &AdapterError) {
        let now = Instant::now();
        let retry = self.sync_retry.entry(session.account).or_insert(Retry {
            first: now,
            attempts: 0,
            due: now,
        });
        retry.attempts = retry.attempts.saturating_add(1);
        let delay = match error {
            AdapterError::RateLimited(delay) => {
                (*delay).clamp(Duration::from_secs(2), Duration::from_secs(3600))
            }
            _ => Duration::from_secs(2u64.pow(retry.attempts.min(6)).min(60)),
        };
        retry.due = now + delay;
        if let Some(entry) = self
            .store
            .accounts
            .iter_mut()
            .find(|a| a.id == session.account)
        {
            entry.connection = ConnectionState::Connecting;
            if retry.first.elapsed() >= Duration::from_secs(120)
                && self.notice.as_ref().is_none_or(|n| n.sync_owner(session))
            {
                self.notice = Some(Notice::sync(session, &entry.label));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prolonged_disconnect_warns_once_and_recovery_clears_only_the_owned_notice() {
        let (mut app, _) = App::new();
        let id = app.store.accounts[0].id;
        let session = app.session(id);
        app.notice = None;
        app.sync_finished(id, Err(AdapterError::Network));
        assert!(app.notice.is_none());
        app.sync_retry.get_mut(&id).unwrap().first = Instant::now() - Duration::from_secs(121);
        app.sync_finished(id, Err(AdapterError::Network));
        assert!(app.notice.as_ref().unwrap().sync_owner(session));
        app.notice.as_mut().unwrap().text = "Different progress wording".into();
        app.sync_finished(id, Ok(SyncBatch::default()));
        assert!(app.notice.is_none());
        let other = app.session(app.store.accounts[1].id);
        for notice in [
            Notice::from("Storage needs attention"),
            Notice::from("Connecting to your account…"),
            Notice::connecting(other),
            Notice::sync(other, "Another account"),
        ] {
            app.notice = Some(notice.clone());
            app.clear_connection_notice(session);
            app.sync_finished(id, Err(AdapterError::Network));
            app.sync_retry.get_mut(&id).unwrap().first = Instant::now() - Duration::from_secs(121);
            app.sync_finished(id, Err(AdapterError::Network));
            app.sync_finished(id, Ok(SyncBatch::default()));
            assert_eq!(app.notice.as_ref(), Some(&notice));
        }
        let mut progress = Notice::connecting(session);
        progress.text = "Different progress wording".into();
        app.notice = Some(progress);
        app.clear_connection_notice(session);
        assert!(app.notice.is_none());
    }
}
