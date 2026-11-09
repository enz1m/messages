use iced::Task;
use messages_core::{AccountId, ConnectionState};
use messages_protocols::login::Secret;

use super::{App, Message, lifecycle::Session};
use crate::platform::credentials::{CredentialIssue, CredentialRef};

pub struct PendingStore {
    session: Session,
    key: CredentialRef,
    token: Secret,
}

impl App {
    pub(super) fn release_credential(&mut self, account: AccountId) {
        if let Some(key) = self.credential_keys.remove(&account) {
            let key = CredentialRef { account, key };
            if !self.credential_cleanup.contains(&key) {
                self.credential_cleanup.push(key);
            }
        }
    }

    pub(super) fn stage_credential(
        &mut self,
        account: AccountId,
        token: Option<String>,
        remember: bool,
    ) {
        self.release_credential(account);
        if remember && let Some(token) = token {
            let key = CredentialRef {
                account,
                key: uuid::Uuid::new_v4(),
            };
            // Persist the key reference before keyring writes so startup can retire unbound keys.
            self.credential_cleanup.push(key);
            self.credential_pending.push(PendingStore {
                session: self.session(account),
                key,
                token: token.into(),
            });
        }
    }

    pub(super) fn credential_tasks(&mut self) -> Task<Message> {
        if self.loading
            || self.persistence_blocked
            || self.saving
            || self.revision != self.saved_revision
        {
            return Task::none();
        }
        let Some(credentials) = self.credentials.clone() else {
            return Task::none();
        };
        let mut tasks = Vec::new();
        for pending in std::mem::take(&mut self.credential_pending) {
            if !self.session_is_current(pending.session) {
                continue;
            }
            self.credential_busy.insert(pending.key);
            let credentials = credentials.clone();
            tasks.push(self.track_job(
                super::jobs::Kind::Credentials,
                async move { credentials.set(pending.key, pending.token).await },
                move |result| Message::CredentialStored(pending.session, pending.key, result),
            ));
        }
        let cleanup: Vec<_> = self
            .credential_cleanup
            .iter()
            .copied()
            .filter(|key| {
                self.credential_keys.get(&key.account) != Some(&key.key)
                    && !self.credential_busy.contains(key)
                    && !self.credential_failed.contains(key)
            })
            .collect();
        for key in cleanup {
            self.credential_busy.insert(key);
            let credentials = credentials.clone();
            tasks.push(self.track_job(
                super::jobs::Kind::Credentials,
                async move { credentials.delete(key).await },
                move |result| Message::CredentialDeleted(key, result),
            ));
        }
        Task::batch(tasks)
    }

    pub(super) fn credential_stored(
        &mut self,
        session: Session,
        key: CredentialRef,
        result: Result<(), CredentialIssue>,
    ) -> Task<Message> {
        self.credential_busy.remove(&key);
        match result {
            Ok(()) if self.session_is_current(session) => {
                self.credential_keys.insert(key.account, key.key);
                self.credential_cleanup.retain(|pending| *pending != key);
                self.credential_notice = Some("Access token saved in the system keyring.".into());
            }
            Ok(()) => {}
            Err(issue) => {
                self.credential_failed.insert(key);
                self.credential_notice = Some(if issue.silent {
                    "Access token is kept for this session only.".into()
                } else {
                    format!(
                        "{} Access token is kept for this session only.",
                        issue.summary
                    )
                });
            }
        }
        self.changed();
        self.save()
    }

    pub(super) fn credential_deleted(
        &mut self,
        key: CredentialRef,
        result: Result<(), CredentialIssue>,
    ) -> Task<Message> {
        self.credential_busy.remove(&key);
        match result {
            Ok(()) => {
                self.credential_cleanup.retain(|pending| *pending != key);
                self.credential_failed.remove(&key);
            }
            Err(issue) => {
                self.credential_failed.insert(key);
                if !issue.silent {
                    self.credential_notice = Some(format!(
                        "{} A retired token still needs keyring cleanup.",
                        issue.summary
                    ));
                }
            }
        }
        self.changed();
        self.save()
    }

    pub(super) fn restore_accounts(&mut self) -> Task<Message> {
        if self.loading || self.demo || self.persistence_blocked || self.stopping() {
            return Task::none();
        }
        let credentials = self.credentials.clone();
        let configs: Vec<_> = self
            .connections
            .iter()
            .filter(|config| {
                !self.adapters.contains_key(&config.account)
                    && !self.credential_restoring.contains(&config.account)
            })
            .cloned()
            .collect();
        let mut tasks = Vec::new();
        for config in configs {
            if self.registry.descriptor(config.protocol.as_str()).is_none() {
                if let Some(account) = self
                    .store
                    .accounts
                    .iter_mut()
                    .find(|a| a.id == config.account)
                {
                    account.capabilities = messages_core::Capabilities::default();
                    account.connection = messages_core::ConnectionState::Failed(
                        "This service is not available in this version. Saved conversations and drafts are preserved.".into(),
                    );
                }
                continue;
            }
            if config.managed.is_some() {
                tasks.push(self.restore_service(config));
                continue;
            }
            let Some(credentials) = credentials.as_ref() else {
                continue;
            };
            let account = config.account;
            let key = self.credential_keys.get(&account).copied();
            if key.is_none() && config.protocol.as_str() != "signal" {
                continue;
            }
            self.renew_session(account);
            self.credential_restoring.insert(account);
            let session = self.session(account);
            if let Some(key) = key {
                let credentials = credentials.clone();
                tasks.push(self.track_job(
                    super::jobs::Kind::Credentials,
                    async move { credentials.get(CredentialRef { account, key }).await },
                    move |result| Message::CredentialLoaded(session, result),
                ));
            } else {
                tasks.push(Task::done(Message::CredentialLoaded(session, Ok(None))));
            }
        }
        Task::batch(tasks)
    }

    pub(super) fn credential_loaded(
        &mut self,
        session: Session,
        result: Result<Option<Secret>, CredentialIssue>,
    ) -> Task<Message> {
        if !self.session_is_current(session) {
            return Task::none();
        }
        // Keep the attempted marker until explicit reconnect; an unavailable or
        // dismissed keyring must not prompt again on every two-second UI tick.
        let Some(config) = self
            .connections
            .iter()
            .find(|config| config.account == session.account)
            .cloned()
        else {
            return Task::none();
        };
        let token = match result {
            Ok(token) => token.map(|token| token.expose().to_owned()),
            Err(issue) => {
                if let Some(account) = self
                    .store
                    .accounts
                    .iter_mut()
                    .find(|account| account.id == session.account)
                {
                    account.connection = ConnectionState::NeedsAuthentication(
                        "Reconnect in Settings to unlock this account.".into(),
                    );
                }
                if !issue.silent {
                    self.credential_notice = Some(issue.summary.into());
                }
                return Task::none();
            }
        };
        match self.registry.connect(config, token) {
            Ok(adapter) => {
                if let Some(account) = self
                    .store
                    .accounts
                    .iter_mut()
                    .find(|account| account.id == session.account)
                {
                    account.connection = ConnectionState::Connecting;
                    account.capabilities = adapter.capabilities();
                }
                self.adapters.insert(session.account, adapter);
                self.sync_account(session.account)
            }
            Err(error) => {
                if let Some(account) = self
                    .store
                    .accounts
                    .iter_mut()
                    .find(|account| account.id == session.account)
                {
                    account.connection = ConnectionState::NeedsAuthentication(error.to_string());
                }
                Task::none()
            }
        }
    }
}
