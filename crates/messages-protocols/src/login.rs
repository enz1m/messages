//! Ephemeral authentication values have redacted Debug output and are never part
//! of serializable account configuration or saved message history.

#[derive(Clone, Default)]
pub struct Secret(String);

impl Secret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl From<String> for Secret {
    fn from(value: String) -> Self {
        Self(value)
    }
}
impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted]")
    }
}

#[derive(Clone, Debug)]
pub enum LoginState {
    Connecting,
    Qr {
        payload: Secret,
        expires_in: u64,
    },
    Phone,
    Code,
    Password,
    MatrixRecovery,
    Matrix {
        password: bool,
        browser: bool,
        problem: Option<String>,
    },
    Browser {
        request_id: uuid::Uuid,
        url: Secret,
        expires_in: u64,
    },
    Ready,
    Unsupported {
        reason: String,
    },
}

#[derive(Clone, Debug)]
pub struct LoginInput {
    pub user: String,
    pub value: Secret,
    pub recover: bool,
}

#[derive(Clone, Debug)]
pub struct BrowserChallenge {
    pub url: Secret,
    pub expires_in: u64,
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum BrowserFailure {
    #[error(
        "This Messages build needs a public HTTPS application address configured before browser sign-in can start. Contact the app developer."
    )]
    AppIdentityMissing,
    #[error(
        "The Matrix server declined Messages as a sign-in app. Its application registration needs to be corrected before browser sign-in can start."
    )]
    RegistrationRejected,
    #[error(
        "This Matrix server requires Messages to be registered by its administrator before browser sign-in can start."
    )]
    RegistrationUnsupported,
    #[error("Browser sign-in could not start. Check your connection and try again.")]
    StartFailed,
}

pub fn validate_browser_url(value: &str) -> Result<(), crate::AdapterError> {
    let uri = url::Url::parse(value)
        .map_err(|_| crate::AdapterError::InvalidData("Invalid browser sign-in URL".into()))?;
    let local = uri.host_str() == Some("localhost")
        || uri
            .host_str()
            .and_then(|s| s.parse::<std::net::IpAddr>().ok())
            .is_some_and(|ip| ip.is_loopback());
    if value.len() > 8192
        || !uri.username().is_empty()
        || uri.password().is_some()
        || uri.fragment().is_some()
        || uri.host_str().is_none()
        || !(uri.scheme() == "https" || (uri.scheme() == "http" && local))
    {
        return Err(crate::AdapterError::InvalidData(
            "Unsafe browser sign-in URL".into(),
        ));
    }
    Ok(())
}
