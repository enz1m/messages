use super::{App, Dialog, Message, jobs, lifecycle::Session};
use iced::Task;
use messages_core::AccountId;
use messages_protocols::{
    Adapter, AdapterError,
    login::{BrowserChallenge, LoginState},
};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone)]
pub(super) struct Lease {
    pub request: Uuid,
    pub session: Session,
    pub adapter: Arc<dyn Adapter>,
}

#[derive(Clone)]
pub struct Completion {
    session: Session,
    showing: Uuid,
    account: AccountId,
    lease: Lease,
    result: Result<BrowserChallenge, AdapterError>,
}
impl std::fmt::Debug for Completion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BrowserCompletion [redacted]")
    }
}

impl App {
    pub(super) fn browser_cleanup(&mut self, previous: Option<Lease>) -> Task<Message> {
        let invalid = self
            .login
            .as_ref()
            .and_then(|login| login.browser.as_ref())
            .is_some_and(|lease| !self.session_is_current(lease.session));
        let close = if invalid {
            self.close_dialog()
        } else {
            Task::none()
        };
        let Some(previous) = previous else {
            return close;
        };
        if self
            .login
            .as_ref()
            .and_then(|login| login.browser.as_ref())
            .is_some_and(|lease| lease.request == previous.request)
        {
            return Task::none();
        }
        self.retire_browser(previous).chain(close)
    }
    fn retire_browser(&self, lease: Lease) -> Task<Message> {
        self.track_job(
            jobs::Kind::AuthenticationCleanup,
            async move {
                let _ = lease.adapter.cancel_browser_login(lease.request).await;
            },
            |()| Message::WorkChanged,
        )
    }
    pub(super) fn begin_browser(&mut self) -> Task<Message> {
        let Some(login) = self.login.as_ref() else {
            return Task::none();
        };
        if self.stopping()
            || self.dialog != Some(Dialog::AccountLogin(login.account))
            || login.busy
            || login.loading
            || login.browser.is_some()
            || !matches!(login.state, LoginState::Matrix { browser: true, .. })
        {
            return Task::none();
        }
        let (account, showing) = (login.account, login.showing);
        let Some(adapter) = self.adapters.get(&account).cloned() else {
            return Task::none();
        };
        let session = self.session(account);
        let lease = Lease {
            request: Uuid::new_v4(),
            session,
            adapter,
        };
        let login = self.login.as_mut().unwrap();
        login.busy = true;
        login.error = None;
        login.value.clear();
        login.browser = Some(lease.clone());
        self.track_job(
            jobs::Kind::Login,
            async move {
                let result = lease.adapter.begin_browser_login(lease.request).await;
                Completion {
                    session,
                    showing,
                    account,
                    lease,
                    result,
                }
            },
            |completion| Message::BrowserCreated(Box::new(completion)),
        )
    }
    pub(super) fn browser_created(&mut self, completion: Completion) -> Task<Message> {
        let current = self.session_is_current(completion.session)
            && self.dialog == Some(Dialog::AccountLogin(completion.account))
            && !self.stopping()
            && self.login.as_ref().is_some_and(|login| {
                login.showing == completion.showing
                    && login
                        .browser
                        .as_ref()
                        .is_some_and(|lease| lease.request == completion.lease.request)
            });
        if !current {
            return self.retire_browser(completion.lease);
        }
        let login = self.login.as_mut().unwrap();
        login.busy = false;
        match completion.result {
            Ok(challenge) => {
                if messages_protocols::login::validate_browser_url(challenge.url.expose()).is_err()
                {
                    login.error = Some("The helper returned an unsafe sign-in URL.".into());
                    login.browser = None;
                    return Task::none();
                }
                login.state = LoginState::Browser {
                    request_id: completion.lease.request,
                    url: challenge.url,
                    expires_in: challenge.expires_in,
                };
                self.open_browser()
            }
            Err(error) => {
                login.error = Some(error.to_string());
                login.browser = None;
                Task::none()
            }
        }
    }
    pub(super) fn open_browser(&mut self) -> Task<Message> {
        let Some(login) = self.login.as_ref() else {
            return Task::none();
        };
        if self.stopping()
            || self.dialog != Some(Dialog::AccountLogin(login.account))
            || !login
                .browser
                .as_ref()
                .is_some_and(|lease| self.session_is_current(lease.session))
        {
            return Task::none();
        }
        let LoginState::Browser { url, .. } = &login.state else {
            return Task::none();
        };
        let uri = url.expose().to_owned();
        let showing = login.showing;
        let account = login.account;
        let session = self.session(account);
        self.track_job(
            jobs::Kind::Login,
            crate::platform::open::web(uri),
            move |result| session.wrap(Message::BrowserOpened(showing, account, result)),
        )
    }
    pub(super) fn browser_opened(
        &mut self,
        showing: Uuid,
        account: AccountId,
        result: Result<(), String>,
    ) {
        if self.dialog == Some(Dialog::AccountLogin(account))
            && let Some(login) = self.login.as_mut().filter(|login| login.showing == showing)
            && let Err(error) = result
        {
            login.error = Some(error);
        }
    }
    pub(super) fn cancel_browser(&mut self) -> Task<Message> {
        let Some(login) = self.login.as_mut() else {
            return Task::none();
        };
        let account = login.account;
        login.browser = None;
        login.busy = false;
        login.loading = false;
        login.showing = Uuid::new_v4();
        login.state = LoginState::Connecting;
        self.load_login(account)
    }
}
