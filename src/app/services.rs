use iced::Task;
use messages_core::{AccountId, ConnectionState};
use messages_protocols::ConnectionConfig;
use uuid::Uuid;

use super::{App, Dialog, Message, lifecycle::Session};
use crate::platform::services::Prepared;

impl App {
    pub(super) fn cancel_service(&mut self) {
        if self.pending_service.take().is_some()
            && let Some(account) = self.account_form.editing
            && let Some(entry) = self
                .store
                .accounts
                .iter_mut()
                .find(|entry| entry.id == account)
        {
            entry.connection = ConnectionState::NeedsAuthentication(
                "Reconnect this account to finish signing in.".into(),
            );
            self.changed();
        }
        self.service_task.take();
    }

    pub(super) fn prepare_service(&mut self, config: ConnectionConfig) -> Task<Message> {
        self.cancel_service();
        self.renew_session(config.account);
        self.adapters.remove(&config.account);
        let generation = Uuid::new_v4();
        self.pending_service = Some(generation);
        let old = self.managed_hosts.remove(&config.account);
        let root = self
            .storage_path
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .to_owned();
        self.notice = None;
        let (task, handle) = self
            .track_job(
                super::jobs::Kind::AccountSetup,
                async move {
                    if let Some(old) = old {
                        old.stop().await;
                    }
                    crate::platform::services::prepare(config, root).await
                },
                move |result| Message::ServicePrepared(generation, result),
            )
            .abortable();
        self.service_task = Some(handle.abort_on_drop());
        task
    }

    pub(super) fn service_prepared(
        &mut self,
        generation: Uuid,
        result: Result<Prepared, String>,
    ) -> Task<Message> {
        if self.pending_service != Some(generation) || self.dialog != Some(Dialog::ConnectAccount) {
            return Task::none();
        }
        self.cancel_service();
        match result {
            Ok(prepared) => {
                self.managed_hosts
                    .insert(prepared.config.account, prepared.host);
                self.install_connection(
                    prepared.config,
                    prepared.token.map(|token| token.expose().to_owned()),
                    false,
                )
            }
            Err(error) => {
                self.notice = Some(error.into());
                Task::none()
            }
        }
    }

    pub(super) fn restore_service(&mut self, config: ConnectionConfig) -> Task<Message> {
        let account = config.account;
        self.renew_session(account);
        self.credential_restoring.insert(account);
        let session = self.session(account);
        let root = self
            .storage_path
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .to_owned();
        self.track_job(
            super::jobs::Kind::AccountSetup,
            crate::platform::services::prepare(config, root),
            move |result| Message::ServiceRestored(session, result),
        )
    }

    pub(super) fn service_restored(
        &mut self,
        session: Session,
        result: Result<Prepared, String>,
    ) -> Task<Message> {
        if !self.session_is_current(session) || self.closing.is_some() {
            return Task::none();
        }
        match result {
            Ok(prepared) => {
                let account = prepared.config.account;
                let token = prepared.token.map(|token| token.expose().to_owned());
                match self.registry.connect(prepared.config.clone(), token) {
                    Ok(adapter) => {
                        self.managed_hosts.insert(account, prepared.host);
                        self.adapters.insert(account, adapter);
                        if let Some(config) = self
                            .connections
                            .iter_mut()
                            .find(|config| config.account == account)
                        {
                            *config = prepared.config;
                        }
                        if let Some(entry) = self
                            .store
                            .accounts
                            .iter_mut()
                            .find(|entry| entry.id == account)
                        {
                            entry.connection = ConnectionState::Connecting;
                        }
                        self.changed();
                        self.credential_restoring.remove(&account);
                        self.managed_login_checks.insert(
                            account,
                            std::time::Instant::now() + std::time::Duration::from_secs(60),
                        );
                        self.restore_login(account)
                    }
                    Err(error) => self.service_failed(account, error.to_string()),
                }
            }
            Err(error) => self.service_failed(session.account, error),
        }
    }

    pub(super) fn restore_login(&mut self, account: AccountId) -> Task<Message> {
        if self.stopping() || !self.sync_due(account) {
            return Task::none();
        }
        if self.credential_restoring.contains(&account) {
            return Task::none();
        }
        let Some(adapter) = self.adapters.get(&account).cloned() else {
            return Task::none();
        };
        self.credential_restoring.insert(account);
        let session = self.session(account);
        self.track_job(
            super::jobs::Kind::Login,
            async move { adapter.login_state().await },
            move |result| Message::ServiceLogin(session, result),
        )
    }

    pub(super) fn restored_login(
        &mut self,
        session: Session,
        result: Result<messages_protocols::login::LoginState, messages_protocols::AdapterError>,
    ) -> Task<Message> {
        use messages_protocols::login::LoginState;
        if !self.session_is_current(session)
            || !self.managed_login_checks.contains_key(&session.account)
        {
            return Task::none();
        }
        let account = session.account;
        self.credential_restoring.remove(&account);
        match result {
            Ok(LoginState::Ready | LoginState::MatrixRecovery) => {
                self.managed_login_checks.remove(&account);
                self.sync_account(account)
            }
            Ok(LoginState::Connecting)
                if self.managed_login_checks[&account] > std::time::Instant::now() =>
            {
                Task::none()
            }
            Ok(LoginState::Connecting) => {
                self.defer_sync(session, &messages_protocols::AdapterError::Network);
                Task::none()
            }
            Ok(_) => {
                self.managed_login_checks.remove(&account);
                if let Some(entry) = self
                    .store
                    .accounts
                    .iter_mut()
                    .find(|entry| entry.id == account)
                {
                    entry.connection = ConnectionState::NeedsAuthentication(
                        "Sign in to finish connecting this account.".into(),
                    );
                }
                if self.dialog.is_none() {
                    self.load_login(account)
                } else {
                    Task::none()
                }
            }
            Err(error) if super::sync::temporary(&error) => {
                self.managed_login_checks.insert(
                    account,
                    std::time::Instant::now() + std::time::Duration::from_secs(60),
                );
                self.defer_sync(session, &error);
                Task::none()
            }
            Err(error) => {
                self.managed_login_checks.remove(&account);
                self.service_failed(account, error.to_string())
            }
        }
    }

    fn service_failed(&mut self, account: AccountId, error: String) -> Task<Message> {
        if let Some(entry) = self
            .store
            .accounts
            .iter_mut()
            .find(|entry| entry.id == account)
        {
            entry.connection = ConnectionState::NeedsAuthentication(error.clone());
            self.notice = Some(format!("{}: {error}", entry.label).into());
        }
        Task::none()
    }
}
