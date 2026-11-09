use matrix_sdk::{
    Client,
    authentication::{
        AuthSession,
        matrix::MatrixSession,
        oauth::{ClientId, OAuthSession, UserSession},
    },
};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StoredSession {
    Matrix {
        session: MatrixSession,
    },
    OAuth {
        client_id: String,
        session: UserSession,
    },
}

pub(super) fn load_session(path: &Path) -> anyhow::Result<AuthSession> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= 1024 * 1024, "Session is too large");
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    if value.get("kind").is_none() {
        return Ok(AuthSession::Matrix(serde_json::from_value(value)?));
    }
    Ok(match serde_json::from_value(value)? {
        StoredSession::Matrix { session } => AuthSession::Matrix(session),
        StoredSession::OAuth { client_id, session } => OAuthSession {
            client_id: ClientId::new(client_id),
            user: session,
        }
        .into(),
    })
}

pub(super) fn save_private_sync(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Missing private state directory"))?;
    let temporary = parent.join(format!(".session-{}", uuid::Uuid::new_v4()));
    struct Staging(PathBuf);
    impl Drop for Staging {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let stage = Staging(temporary);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&stage.0)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&stage.0, path)?;
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

pub(super) fn save_session(client: &Client, path: &Path, lock: &Mutex<()>) -> anyhow::Result<()> {
    let _guard = lock
        .lock()
        .map_err(|_| anyhow::anyhow!("Session writer unavailable"))?;
    let session = match client
        .session()
        .ok_or_else(|| anyhow::anyhow!("Session unavailable"))?
    {
        AuthSession::Matrix(session) => StoredSession::Matrix { session },
        AuthSession::OAuth(session) => StoredSession::OAuth {
            client_id: session.client_id.as_str().to_owned(),
            session: session.user,
        },
        _ => anyhow::bail!("Unsupported authentication session"),
    };
    save_private_sync(path, &serde_json::to_vec(&session)?)
}

pub(super) fn session_callbacks(
    client: &Client,
    path: PathBuf,
    lock: Arc<Mutex<()>>,
) -> anyhow::Result<()> {
    let reload = path.clone();
    client.set_session_callbacks(
        Box::new(move |_| {
            let session = load_session(&reload)
                .map_err(|_| std::io::Error::other("Stored session could not be read"))?;
            Ok(matrix_sdk::SessionTokens {
                access_token: session.access_token().to_owned(),
                refresh_token: session.get_refresh_token().map(str::to_owned),
            })
        }),
        Box::new(move |client| {
            save_session(&client, &path, &lock)
                .map_err(|_| std::io::Error::other("Session could not be saved").into())
        }),
    )?;
    Ok(())
}

#[derive(Clone, Copy, Default)]
pub(super) struct Methods {
    pub password: bool,
    pub legacy: bool,
    pub oauth: bool,
}

pub(super) async fn discover(client: &Client) -> anyhow::Result<Methods> {
    use matrix_sdk::ruma::api::client::session::get_login_types::v3::LoginType;
    let oauth = tokio::time::timeout(
        std::time::Duration::from_secs(8),
        client.oauth().cached_server_metadata(),
    )
    .await;
    let oauth = matches!(oauth, Ok(Ok(_)));
    let mut methods = Methods {
        oauth,
        ..Default::default()
    };
    if let Ok(Ok(response)) = tokio::time::timeout(
        std::time::Duration::from_secs(8),
        client.matrix_auth().get_login_types(),
    )
    .await
    {
        methods.password = response
            .flows
            .iter()
            .any(|flow| matches!(flow, LoginType::Password(_)));
        methods.legacy = response
            .flows
            .iter()
            .any(|flow| matches!(flow, LoginType::Sso(_)));
    }
    anyhow::ensure!(
        methods.password || methods.legacy || methods.oauth,
        "No supported authentication flow was discovered"
    );
    Ok(methods)
}
