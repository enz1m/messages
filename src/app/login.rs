use enzui_iced_widgets::qr::EnzQrData;
use iced::Task;
use messages_core::AccountId;
use messages_protocols::{
    AdapterError,
    login::{LoginInput, LoginState},
};

use super::{App, Dialog, Message};

pub struct Login {
    pub showing: uuid::Uuid,
    pub(super) browser: Option<super::browser_login::Lease>,
    pub account: AccountId,
    pub state: LoginState,
    pub qr: Option<EnzQrData>,
    pub qr_expires: Option<std::time::Instant>,
    pub user: String,
    pub value: String,
    pub busy: bool,
    pub loading: bool,
    pub error: Option<String>,
}

impl App {
    pub(super) fn load_login(&mut self, account: AccountId) -> Task<Message> {
        if self.stopping() {
            return Task::none();
        }
        let Some(adapter) = self.adapters.get(&account).cloned() else {
            return self.reconnect(account);
        };
        if self
            .login
            .as_ref()
            .is_none_or(|login| login.account != account)
        {
            self.login = Some(Login {
                showing: uuid::Uuid::new_v4(),
                browser: None,
                account,
                state: LoginState::Connecting,
                qr: None,
                qr_expires: None,
                user: String::new(),
                value: String::new(),
                busy: false,
                loading: false,
                error: None,
            });
        }
        let login = self.login.as_mut().expect("login initialized above");
        if login.loading || login.busy || matches!(login.state, LoginState::MatrixRecovery) {
            return Task::none();
        }
        login.loading = true;
        let showing = login.showing;
        let open = if self.dialog != Some(Dialog::AccountLogin(account)) {
            self.open_dialog(Dialog::AccountLogin(account))
        } else {
            Task::none()
        };
        let session = self.session(account);
        Task::batch([
            open,
            self.track_job(
                super::jobs::Kind::Login,
                async move { adapter.login_state().await },
                move |result| session.wrap(Message::LoginLoaded(showing, account, result)),
            ),
        ])
    }

    pub(super) fn login_loaded(
        &mut self,
        showing: uuid::Uuid,
        account: AccountId,
        result: Result<LoginState, AdapterError>,
    ) -> Task<Message> {
        if self.closing.is_some() || self.dialog != Some(Dialog::AccountLogin(account)) {
            return Task::none();
        }
        let session = self.session(account);
        let Some(login) = self
            .login
            .as_mut()
            .filter(|login| login.account == account && login.showing == showing)
        else {
            return Task::none();
        };
        login.loading = false;
        match result {
            Ok(state) => {
                let ready = matches!(state, LoginState::Ready);
                if let LoginState::Qr {
                    payload,
                    expires_in,
                } = &state
                {
                    login.qr = EnzQrData::try_new(payload.expose()).ok();
                    login.qr_expires = Some(
                        std::time::Instant::now() + std::time::Duration::from_secs(*expires_in),
                    );
                } else {
                    login.qr = None;
                    login.qr_expires = None;
                }
                if let LoginState::Browser { request_id, .. } = &state
                    && login.browser.is_none()
                    && let Some(adapter) = self.adapters.get(&account).cloned()
                {
                    login.browser = Some(super::browser_login::Lease {
                        request: *request_id,
                        session,
                        adapter,
                    });
                }
                if !matches!(state, LoginState::Browser { .. }) && !login.busy {
                    login.browser = None;
                }
                login.state = state;
                self.clear_connection_notice(self.session(account));
                if ready {
                    self.login = None;
                    return Task::batch([self.close_dialog(), self.sync_account(account)]);
                }
            }
            Err(AdapterError::Authentication) => return self.reconnect(account),
            Err(error) => login.error = Some(error.to_string()),
        }
        Task::none()
    }

    pub(super) fn submit_login(&mut self) -> Task<Message> {
        let Some(login) = &mut self.login else {
            return Task::none();
        };
        if login.busy
            || login.loading
            || login.value.is_empty()
            || !matches!(
                login.state,
                LoginState::Phone
                    | LoginState::Code
                    | LoginState::Password
                    | LoginState::Matrix { password: true, .. }
                    | LoginState::MatrixRecovery
            )
        {
            return Task::none();
        }
        let account = login.account;
        let showing = login.showing;
        let Some(adapter) = self.adapters.get(&account).cloned() else {
            return Task::none();
        };
        let input = LoginInput {
            user: login.user.clone(),
            value: std::mem::take(&mut login.value).into(),
            recover: matches!(login.state, LoginState::MatrixRecovery),
        };
        login.busy = true;
        login.error = None;
        let session = self.session(account);
        self.track_job(
            super::jobs::Kind::Login,
            async move { adapter.authenticate(input).await },
            move |result| session.wrap(Message::LoginSubmitted(showing, account, result)),
        )
    }

    pub(super) fn restore_matrix_keys(&mut self, account: AccountId) -> Task<Message> {
        if self.stopping()
            || !self
                .adapters
                .get(&account)
                .is_some_and(|adapter| adapter.supports_encryption_recovery())
        {
            return Task::none();
        }
        self.login = Some(Login {
            showing: uuid::Uuid::new_v4(),
            browser: None,
            account,
            state: LoginState::MatrixRecovery,
            qr: None,
            qr_expires: None,
            user: String::new(),
            value: String::new(),
            busy: false,
            loading: false,
            error: None,
        });
        self.open_dialog(Dialog::AccountLogin(account))
    }
}
