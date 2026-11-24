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

const LEGACY_SESSION: &str = "session.json";
const SEALED_SESSION: &str = "session.sealed";
const MAX_SESSION_BYTES: usize = 1024 * 1024;
const NONCE_BYTES: usize = 24;

/// The account's stored session tokens.
///
/// With a store key the file is sealed with XChaCha20-Poly1305 under a key
/// derived from it; without one the session is a plain private file.
#[derive(Clone)]
pub(crate) struct SessionFile {
    path: PathBuf,
    seal: Option<[u8; 32]>,
}

impl SessionFile {
    pub(crate) fn new(directory: &Path, store_key: Option<&str>) -> Self {
        use sha2::Digest;
        match store_key {
            Some(key) => Self {
                path: directory.join(SEALED_SESSION),
                seal: Some(
                    sha2::Sha256::new()
                        .chain_update(b"io.enz.Messages matrix session\0")
                        .chain_update(key.as_bytes())
                        .finalize()
                        .into(),
                ),
            },
            None => Self {
                path: directory.join(LEGACY_SESSION),
                seal: None,
            },
        }
    }

    /// Seals a plain session left in the directory and removes the plain copy.
    pub(crate) fn migrate(&self) -> anyhow::Result<()> {
        if self.seal.is_none() {
            return Ok(());
        }
        let legacy = self.path.with_file_name(LEGACY_SESSION);
        let bytes = match read_limited(&legacy, MAX_SESSION_BYTES) {
            Ok(bytes) => bytes,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        if !self.path.exists() {
            parse_session(&bytes)?;
            self.write(&bytes)?;
        }
        std::fs::remove_file(&legacy)?;
        Ok(())
    }

    /// The stored session; a missing file is an [`std::io::ErrorKind::NotFound`] error.
    pub(crate) fn load(&self) -> anyhow::Result<AuthSession> {
        let Some(key) = &self.seal else {
            return parse_session(&read_limited(&self.path, MAX_SESSION_BYTES)?);
        };
        use chacha20poly1305::{
            XChaCha20Poly1305, XNonce,
            aead::{Aead, KeyInit},
        };
        let sealed = read_limited(&self.path, MAX_SESSION_BYTES + NONCE_BYTES + 16)?;
        anyhow::ensure!(sealed.len() > NONCE_BYTES, "Stored session is invalid");
        let (nonce, ciphertext) = sealed.split_at(NONCE_BYTES);
        let nonce =
            XNonce::try_from(nonce).map_err(|_| anyhow::anyhow!("Stored session is invalid"))?;
        let bytes = XChaCha20Poly1305::new(&(*key).into())
            .decrypt(&nonce, ciphertext)
            .map_err(|_| anyhow::anyhow!("Stored session could not be decrypted"))?;
        parse_session(&bytes)
    }

    fn write(&self, bytes: &[u8]) -> anyhow::Result<()> {
        let Some(key) = &self.seal else {
            return save_private_sync(&self.path, bytes);
        };
        use chacha20poly1305::{
            XChaCha20Poly1305, XNonce,
            aead::{Aead, Generate, KeyInit},
        };
        let nonce = XNonce::generate();
        let ciphertext = XChaCha20Poly1305::new(&(*key).into())
            .encrypt(&nonce, bytes)
            .map_err(|_| anyhow::anyhow!("Session could not be sealed"))?;
        save_private_sync(&self.path, &[nonce.as_slice(), &ciphertext].concat())
    }
}

fn read_limited(path: &Path, limit: usize) -> anyhow::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= limit, "Session is too large");
    Ok(bytes)
}

fn parse_session(bytes: &[u8]) -> anyhow::Result<AuthSession> {
    let value: serde_json::Value = serde_json::from_slice(bytes)?;
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

pub(super) fn save_session(
    client: &Client,
    file: &SessionFile,
    lock: &Mutex<()>,
) -> anyhow::Result<()> {
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
    file.write(&serde_json::to_vec(&session)?)
}

pub(super) fn session_callbacks(
    client: &Client,
    file: SessionFile,
    lock: Arc<Mutex<()>>,
) -> anyhow::Result<()> {
    let reload = file.clone();
    client.set_session_callbacks(
        Box::new(move |_| {
            let session = reload
                .load()
                .map_err(|_| std::io::Error::other("Stored session could not be read"))?;
            Ok(matrix_sdk::SessionTokens {
                access_token: session.access_token().to_owned(),
                refresh_token: session.get_refresh_token().map(str::to_owned),
            })
        }),
        Box::new(move |client| {
            save_session(&client, &file, &lock)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_session_is_sealed_under_the_store_key_without_losing_the_plain_session() {
        let directory = tempfile::tempdir().unwrap();
        let legacy = directory.path().join(LEGACY_SESSION);
        std::fs::write(
            &legacy,
            br#"{"user_id":"@you:example.test","device_id":"FIXTURE","access_token":"fictional-matrix-token"}"#,
        )
        .unwrap();
        let file = SessionFile::new(directory.path(), Some("fixture-store-key-0123456789abcdef"));
        file.migrate().unwrap();
        assert!(!legacy.exists());
        let sealed = std::fs::read(directory.path().join(SEALED_SESSION)).unwrap();
        assert!(!sealed.windows(22).any(|w| w == b"fictional-matrix-token"));
        assert_eq!(
            file.load().unwrap().access_token(),
            "fictional-matrix-token"
        );
        let other = SessionFile::new(directory.path(), Some("another-store-key-0123456789abcdef"));
        assert!(other.load().is_err());
    }
}
