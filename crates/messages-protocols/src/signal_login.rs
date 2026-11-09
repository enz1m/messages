use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

use reqwest::Method;
use serde_json::{Value, json};
use tokio::task::JoinHandle;

use crate::{
    AdapterError,
    http::Http,
    login::{LoginState, Secret},
    signal::Signal,
};

#[derive(Default)]
pub(crate) struct SignalAuth {
    pub user: Option<String>,
    qr: Option<(Secret, Instant)>,
    task: Option<JoinHandle<Result<String, AdapterError>>>,
}

impl Drop for SignalAuth {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

pub(crate) fn auth() -> Mutex<SignalAuth> {
    Mutex::new(SignalAuth::default())
}

impl Signal {
    pub(crate) fn active_user(&self) -> Result<String, AdapterError> {
        let state = self.auth.lock().map_err(|_| {
            AdapterError::InvalidData("Signal authentication state unavailable".into())
        })?;
        state
            .user
            .clone()
            .or_else(|| (!self.config.user_id.is_empty()).then(|| self.config.user_id.clone()))
            .ok_or(AdapterError::Authentication)
    }

    pub(crate) async fn link_state(&self) -> Result<LoginState, AdapterError> {
        let _gate = self.login_gate.lock().await;
        let finished = {
            let mut state = self.auth.lock().map_err(|_| {
                AdapterError::InvalidData("Signal authentication state unavailable".into())
            })?;
            if state.user.is_some() {
                return Ok(LoginState::Ready);
            }
            if state.task.as_ref().is_some_and(JoinHandle::is_finished) {
                state.task.take()
            } else {
                None
            }
        };
        if let Some(task) = finished {
            let result = task.await.unwrap_or(Err(AdapterError::Network));
            self.auth.lock().map_err(|_| AdapterError::Network)?.qr = None;
            let user = match result {
                Ok(user) => user,
                Err(error) => {
                    // Linking can commit before the helper's final response fails.
                    let confirmed = self.registered_user().await.ok().flatten();
                    confirmed.ok_or(error)?
                }
            };
            self.auth.lock().map_err(|_| AdapterError::Network)?.user = Some(user);
            return Ok(LoginState::Ready);
        }
        {
            let mut state = self.auth.lock().map_err(|_| {
                AdapterError::InvalidData("Signal authentication state unavailable".into())
            })?;
            if let Some((payload, expiry)) = &state.qr {
                if let Some(remaining) = expiry.checked_duration_since(Instant::now()) {
                    return Ok(LoginState::Qr {
                        payload: payload.clone(),
                        expires_in: remaining.as_secs().max(1),
                    });
                }
                if let Some(task) = state.task.take() {
                    task.abort();
                }
                state.qr = None;
            }
        }
        if let Some(user) = self.registered_user().await? {
            self.auth.lock().map_err(|_| AdapterError::Network)?.user = Some(user);
            return Ok(LoginState::Ready);
        }
        let result = rpc(&self.http, "startLink", json!({}), Duration::from_secs(15)).await?;
        let uri = result["deviceLinkUri"]
            .as_str()
            .filter(|uri| uri.starts_with("sgnl://linkdevice?"))
            .ok_or_else(|| AdapterError::InvalidData("Signal pairing link missing".into()))?
            .to_owned();
        let http = self.http.clone();
        let payload: Secret = uri.clone().into();
        let task = tokio::spawn(async move {
            let account = rpc(
                &http,
                "finishLink",
                json!({"deviceLinkUri":uri,"deviceName":"Messages"}),
                Duration::from_secs(120),
            )
            .await?;
            identifier(&account).ok_or_else(|| {
                AdapterError::InvalidData("Linked Signal account identity missing".into())
            })
        });
        let mut state = self.auth.lock().map_err(|_| AdapterError::Network)?;
        state.qr = Some((payload.clone(), Instant::now() + Duration::from_secs(90)));
        state.task = Some(task);
        Ok(LoginState::Qr {
            payload,
            expires_in: 90,
        })
    }

    async fn registered_user(&self) -> Result<Option<String>, AdapterError> {
        let accounts = rpc(
            &self.http,
            "listAccounts",
            json!({}),
            Duration::from_secs(15),
        )
        .await?;
        let accounts = accounts
            .as_array()
            .ok_or_else(|| AdapterError::InvalidData("Signal account list missing".into()))?;
        let account = accounts.iter().find(|account| {
            self.config.user_id.is_empty()
                || identifier(account).as_deref() == Some(&self.config.user_id)
        });
        account
            .map(|account| {
                identifier(account).ok_or_else(|| {
                    AdapterError::InvalidData("Signal account identity missing".into())
                })
            })
            .transpose()
    }
}

fn identifier(value: &Value) -> Option<String> {
    value["number"]
        .as_str()
        .filter(|number| !number.is_empty())
        .or(value["aci"].as_str())
        .map(str::to_owned)
}

async fn rpc(
    http: &Http,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value, AdapterError> {
    let id = uuid::Uuid::new_v4().to_string();
    let response = http
        .request(Method::POST, http.url(&["api", "v1", "rpc"])?)
        .timeout(timeout)
        .json(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
        .send()
        .await
        .map_err(|_| AdapterError::Network)?;
    let value = Http::decode(response).await?;
    if value["id"] != id || !value["error"].is_null() {
        return Err(AdapterError::InvalidData(format!(
            "Signal {method} could not be completed."
        )));
    }
    value
        .get("result")
        .cloned()
        .ok_or_else(|| AdapterError::InvalidData("Signal response missing a result".into()))
}
