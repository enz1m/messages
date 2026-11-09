use super::{Matrix, auth, callback};
use matrix_sdk::{
    Client,
    authentication::oauth::{CsrfToken, registration::ClientMetadata},
    ruma::serde::Raw,
};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{
        Arc, Mutex as FileMutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    net::TcpListener,
    sync::{Mutex, Notify},
    task::JoinHandle,
};
use url::Url;
use uuid::Uuid;

const LIFETIME: Duration = Duration::from_secs(600);

#[derive(Default)]
pub(super) struct Browser {
    active: Option<Active>,
    retired: HashSet<Uuid>,
    blocked: bool,
    problem: Option<&'static str>,
}
struct Active {
    request: Uuid,
    url: String,
    deadline: Instant,
    finished: bool,
    csrf: Option<CsrfToken>,
    _task: TaskOwner,
}
struct TaskOwner(JoinHandle<()>);
impl Drop for TaskOwner {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl Browser {
    pub fn status(&self) -> Option<Value> {
        self.active.as_ref().filter(|a|!a.finished&&a.deadline>Instant::now()).map(|a|json!({"state":"browser","request_id":a.request,"url":a.url,"expires_in":a.deadline.saturating_duration_since(Instant::now()).as_secs()}))
    }
    pub fn problem(&self) -> Option<&'static str> {
        self.problem
    }
}

struct Completion {
    client: Client,
    directory: PathBuf,
    file_lock: Arc<FileMutex<()>>,
    writes: Arc<Mutex<()>>,
    committed: Arc<AtomicBool>,
    ready: Arc<Notify>,
    browser: Weak<Mutex<Browser>>,
}

impl Matrix {
    pub(super) async fn methods(&self) -> anyhow::Result<auth::Methods> {
        let mut cached = self.methods.lock().await;
        if let Some((time, methods)) = *cached
            && time.elapsed() < Duration::from_secs(60)
        {
            return Ok(methods);
        }
        let mut methods = auth::discover(&self.client).await?;
        if methods.oauth {
            methods.password = false;
        }
        *cached = Some((Instant::now(), methods));
        Ok(methods)
    }

    pub(super) async fn start_browser(&self, request: Uuid) -> anyhow::Result<Value> {
        let _write = self.writes.lock().await;
        anyhow::ensure!(
            !self.committed.load(Ordering::Acquire),
            "This account is already signed in"
        );
        {
            let browser = self.browser.lock().await;
            anyhow::ensure!(
                !browser.blocked && !browser.retired.contains(&request),
                "This browser request was cancelled"
            );
            if let Some(active) = &browser.active
                && !active.finished
                && active.deadline > Instant::now()
            {
                anyhow::ensure!(active.request == request, "A sign-in is already active");
                return Ok(browser.status().unwrap());
            }
        }
        let methods = self.methods().await?;
        anyhow::ensure!(
            methods.oauth || methods.legacy,
            "This server does not offer browser sign-in"
        );
        let client_uri = self
            .client_uri
            .as_deref()
            .and_then(|value| Url::parse(value).ok())
            .filter(|uri| {
                uri.scheme() == "https"
                    && uri.host_str().is_some_and(|host| {
                        host != "localhost"
                            && !host.ends_with(".localhost")
                            && host.parse::<std::net::IpAddr>().is_err()
                    })
                    && uri.username().is_empty()
                    && uri.password().is_none()
                    && uri.query().is_none()
                    && uri.fragment().is_none()
            });
        let use_oauth = methods.oauth && client_uri.is_some();
        if !use_oauth && !methods.legacy {
            return Err(messages_protocols::login::BrowserFailure::AppIdentityMissing.into());
        }
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let path = if use_oauth {
            "auth".to_owned()
        } else {
            format!("auth/{}", Uuid::new_v4().simple())
        };
        let redirect = Url::parse(&format!("http://{}/{path}", listener.local_addr()?))?;
        let (url, csrf) = if use_oauth {
            let client_uri = client_uri.expect("OAuth requires a validated application identity");
            let metadata:Raw<ClientMetadata>=Raw::from_json_string(json!({
                "application_type":"native", "client_name":"Messages",
                "client_uri":client_uri.as_str(),
                "redirect_uris":[redirect.as_str()], "grant_types":["authorization_code","refresh_token"],
                "response_types":["code"],"token_endpoint_auth_method":"none"
            }).to_string())?;
            let data = self
                .client
                .oauth()
                .login(redirect.clone(), None, Some(metadata.into()), None)
                .build()
                .await
                .map_err(|error| {
                    use matrix_sdk::authentication::oauth::{
                        OAuthError,
                        error::{OAuthClientRegistrationError, RequestTokenError},
                    };
                    use messages_protocols::login::BrowserFailure;
                    match error {
                        OAuthError::ClientRegistration(
                            OAuthClientRegistrationError::NotSupported,
                        ) => BrowserFailure::RegistrationUnsupported,
                        OAuthError::ClientRegistration(OAuthClientRegistrationError::OAuth(
                            RequestTokenError::ServerResponse(_),
                        )) => BrowserFailure::RegistrationRejected,
                        _ => BrowserFailure::StartFailed,
                    }
                })?;
            (data.url.to_string(), Some(data.state))
        } else {
            (
                self.client
                    .matrix_auth()
                    .get_sso_login_url(redirect.as_str(), None)
                    .await?,
                None,
            )
        };
        let completion = Completion {
            client: self.client.clone(),
            directory: self.directory.clone(),
            file_lock: self.file_lock.clone(),
            writes: self.writes.clone(),
            committed: self.committed.clone(),
            ready: self.ready.clone(),
            browser: Arc::downgrade(&self.browser),
        };
        let mut browser = self.browser.lock().await;
        if browser.blocked || browser.retired.contains(&request) {
            drop(browser);
            if let Some(csrf) = &csrf {
                self.client.oauth().abort_login(csrf).await;
            }
            anyhow::bail!("This browser request was cancelled");
        }
        let oauth = csrf.is_some();
        let cleanup_csrf = csrf.clone();
        let expected_state = csrf.as_ref().map(|state| state.secret().to_owned());
        let cleanup_client = self.client.clone();
        let task = tokio::spawn(async move {
            let result = complete(
                completion,
                listener,
                redirect,
                request,
                oauth,
                expected_state,
            )
            .await;
            if result.is_err()
                && let Some(state) = cleanup_csrf
            {
                // The SDK owns validation state; dropping a socket does not retire its PKCE verifier.
                cleanup_client.oauth().abort_login(&state).await;
            }
        });
        browser.active = Some(Active {
            request,
            url: url.clone(),
            deadline: Instant::now() + LIFETIME,
            finished: false,
            csrf,
            _task: TaskOwner(task),
        });
        browser.problem = None;
        Ok(
            json!({"state":"browser","request_id":request,"url":url,"expires_in":LIFETIME.as_secs()}),
        )
    }

    pub(super) async fn cancel_browser(&self, request: Uuid) -> anyhow::Result<()> {
        let mut browser = self.browser.lock().await;
        if browser.retired.len() >= 128 {
            browser.blocked = true;
        } else {
            browser.retired.insert(request);
        }
        let active = if browser
            .active
            .as_ref()
            .is_some_and(|a| a.request == request)
        {
            browser.active.take()
        } else {
            None
        };
        if active.is_some() {
            browser.problem = None;
        }
        drop(browser);
        if let Some(mut active) = active {
            let csrf = active.csrf.clone();
            active._task.0.abort();
            let _ = (&mut active._task.0).await;
            drop(active);
            if let Some(csrf) = csrf {
                self.client.oauth().abort_login(&csrf).await;
            }
        }
        Ok(())
    }
}

async fn complete(
    context: Completion,
    listener: TcpListener,
    redirect: Url,
    request: Uuid,
    oauth: bool,
    expected_state: Option<String>,
) -> anyhow::Result<()> {
    let outcome = tokio::time::timeout(
        LIFETIME,
        callback::receive(&listener, &redirect, expected_state.as_deref()),
    )
    .await;
    let result = async {
        let (mut stream, uri) = outcome??;
        let _write = context.writes.lock().await;
        let state = context
            .browser
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("Sign-in retired"))?;
        let current = state.lock().await;
        anyhow::ensure!(
            !current.retired.contains(&request)
                && current
                    .active
                    .as_ref()
                    .is_some_and(|a| a.request == request),
            "Sign-in retired"
        );
        drop(current);
        let authenticated = if oauth {
            context
                .client
                .oauth()
                .finish_login(uri.into())
                .await
                .map(|_| ())
        } else {
            let tokens = uri
                .query_pairs()
                .filter(|(key, _)| key == "loginToken")
                .map(|(_, value)| value.into_owned())
                .collect::<Vec<_>>();
            anyhow::ensure!(
                tokens.len() == 1 && !tokens[0].is_empty(),
                "Missing login token"
            );
            context
                .client
                .matrix_auth()
                .login_token(&tokens[0])
                .initial_device_display_name("Messages")
                .request_refresh_token()
                .await
                .map(|_| ())
        };
        if authenticated.is_err() {
            let _ = callback::respond(&mut stream, false).await;
            anyhow::bail!("Browser authentication failed");
        }
        let again = state.lock().await;
        anyhow::ensure!(
            !again.retired.contains(&request)
                && again.active.as_ref().is_some_and(|a| a.request == request),
            "Sign-in retired"
        );
        drop(again);
        auth::save_session(
            &context.client,
            &context.directory.join("session.json"),
            &context.file_lock,
        )?;
        context.committed.store(true, Ordering::Release);
        context.ready.notify_one();
        let _ = callback::respond(&mut stream, true).await;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    if let Some(state) = context.browser.upgrade() {
        let mut state = state.lock().await;
        if let Some(active) = &mut state.active
            && active.request == request
        {
            active.finished = true;
        }
        if result.is_err() {
            state.problem = Some("Browser sign-in failed or expired. Try again.");
        }
    }
    result
}
