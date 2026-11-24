use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

use messages_protocols::{
    ConnectionConfig, ManagedConnection, local_socket::LocalSocket, login::Secret,
};
use tokio::process::{Child, Command};

use super::credentials::{CredentialIssue, Credentials};

const SOCKET_NAME: &str = "helper.sock";

pub struct Host {
    child: Mutex<Option<Child>>,
}

impl std::fmt::Debug for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ManagedHost")
    }
}

impl Host {
    /// A host owning an already running child, for tests that need a live
    /// helper without the real service programs.
    #[cfg(test)]
    pub fn from_child(child: Child) -> Self {
        Self {
            child: Mutex::new(Some(child)),
        }
    }

    /// Whether the helper process has been stopped and reaped.
    #[cfg(test)]
    pub fn is_stopped(&self) -> bool {
        self.child.lock().is_ok_and(|slot| slot.is_none())
    }

    pub async fn stop(&self) {
        let child = self.child.lock().ok().and_then(|mut slot| slot.take());
        if let Some(mut child) = child {
            if let Some(pid) = child.id() {
                let _ = Command::new("/bin/kill")
                    .arg("-INT")
                    .arg(pid.to_string())
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .await;
            }
            if tokio::time::timeout(Duration::from_secs(5), child.wait())
                .await
                .is_err()
            {
                let _ = child.start_kill();
                let _ = child.wait().await;
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct Prepared {
    pub config: ConnectionConfig,
    pub token: Option<Secret>,
    pub host: Arc<Host>,
}

pub async fn prepare(
    config: ConnectionConfig,
    root: PathBuf,
    credentials: Option<Arc<Credentials>>,
) -> Result<Prepared, String> {
    let executable = std::env::current_exe().map_err(|_| "Could not locate the application.")?;
    let bin_directory = std::env::var_os("MESSAGES_HELPER_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| executable.parent().unwrap_or(Path::new(".")).to_owned());
    prepare_with_programs(config, root, credentials, bin_directory, None).await
}

async fn prepare_with_programs(
    mut config: ConnectionConfig,
    root: PathBuf,
    credentials: Option<Arc<Credentials>>,
    bin_directory: PathBuf,
    signal_program: Option<PathBuf>,
) -> Result<Prepared, String> {
    let service = config.managed.clone().ok_or("Choose a managed service.")?;
    if matches!(service, ManagedConnection::Retired { .. }) {
        return Err(
            "This connection is not available in this version. Its saved data is preserved.".into(),
        );
    }
    private_directory(&root.join("services")).await?;
    let directory = root.join("services").join(config.account.to_string());
    private_directory(&directory).await?;
    let socket_path = directory.join(SOCKET_NAME);
    match tokio::fs::remove_file(&socket_path).await {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err("Could not prepare private service storage.".into());
        }
        _ => {}
    }
    let endpoint = LocalSocket::endpoint(&socket_path)
        .ok_or("The service storage path is not valid Unicode.")?;
    let store_key = match (&service, credentials) {
        (ManagedConnection::Matrix { .. }, Some(credentials)) => {
            Some(matrix_store_key(&credentials, config.account, &directory).await?)
        }
        (ManagedConnection::Matrix { .. }, None) => {
            return Err("The system keyring is not ready yet. Try connecting again.".into());
        }
        _ => None,
    };
    let mut command = match &service {
        ManagedConnection::Signal => {
            let path = signal_program.unwrap_or_else(|| bin_directory.join("signal-cli"));
            if !path.is_file() {
                return Err(
                    "The bundled Signal runtime is missing. Repair the Messages installation."
                        .into(),
                );
            }
            let mut command = Command::new(path);
            command
                .arg("--data-dir")
                .arg(directory.join("signal"))
                .arg("daemon")
                .arg("--receive-mode=manual")
                .arg("--no-receive-stdout")
                // signal-cli creates the parent of the socket path, so the name needs one.
                .arg(format!("--socket=./{SOCKET_NAME}"));
            command
        }
        _ => {
            let name = match &service {
                ManagedConnection::WhatsApp => "messages-whatsapp",
                ManagedConnection::Matrix { .. } => "messages-matrix",
                ManagedConnection::Signal | ManagedConnection::Retired { .. } => unreachable!(),
            };
            let path = bin_directory.join(name);
            if !path.is_file() {
                return Err(format!(
                    "The {name} helper is missing. Build or install the native service helpers."
                ));
            }
            let mut command = Command::new(path);
            command
                .env("MESSAGES_BRIDGE_STATE", &directory)
                .env("MESSAGES_BRIDGE_SOCKET", SOCKET_NAME);
            if let ManagedConnection::Matrix { homeserver } = &service {
                messages_protocols::http::Http::new(homeserver, None)
                    .map_err(|error| error.to_string())?;
                command
                    .env("MATRIX_HOMESERVER", homeserver)
                    .env("MESSAGES_MATRIX_KEY_STDIN", "1");
            }
            command
        }
    };
    // Helpers bind a relative name so deep state paths fit the Unix socket path limit.
    let mut child = command
        .current_dir(&directory)
        .stdin(if store_key.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| {
            if matches!(service, ManagedConnection::Signal) {
                "The bundled Signal runtime could not start. Repair the Messages installation."
                    .to_owned()
            } else {
                "Could not start the native service helper.".to_owned()
            }
        })?;
    if let (Some(key), Some(mut stdin)) = (store_key, child.stdin.take()) {
        use tokio::io::AsyncWriteExt;
        stdin
            .write_all(key.expose().as_bytes())
            .await
            .map_err(|_| "Could not start the native service helper.")?;
    }
    let pid = child.id().ok_or("Could not check the service process.")?;
    config.endpoint = endpoint;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    let token = loop {
        if child
            .try_wait()
            .map_err(|_| "Could not check the service process.")?
            .is_some()
        {
            return Err("The service helper exited before it was ready. Check its installation and account settings.".into());
        }
        let serving = served_by(&socket_path, pid).await;
        if serving && matches!(service, ManagedConnection::Signal) {
            break None;
        } else if serving
            && let Ok(token) = tokio::fs::read_to_string(directory.join("access-token")).await
            && (16..=4096).contains(&token.len())
        {
            let bridge =
                messages_protocols::bridge::Bridge::new(config.clone(), Some(token.clone()))
                    .map_err(|error| error.to_string())?;
            if let Ok(status) = tokio::time::timeout(Duration::from_secs(1), bridge.status()).await
                && status.is_ok()
            {
                break Some(token.into());
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("The service helper did not become ready. Try connecting again.".into());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    Ok(Prepared {
        config,
        token,
        host: Arc::new(Host {
            child: Mutex::new(Some(child)),
        }),
    })
}

/// The account-owned directories under the state root: the helper's SDK
/// databases, keys and configuration, and the staged/opened attachments.
pub fn account_directories(root: &Path, account: uuid::Uuid) -> [PathBuf; 2] {
    let name = account.to_string();
    [
        root.join("services").join(&name),
        root.join("files").join(name),
    ]
}

/// Erase a removed account's helper storage and attachment files.
///
/// Callers hold the stop/settlement barrier: the account's helper is stopped
/// and no setup or file work for it can still be writing. A directory that is
/// already gone counts as erased; a symbolic link in its place is removed
/// without following it. Success means both names are verified absent. A
/// Matrix store key is then deleted from the keyring when one is reachable.
pub async fn erase_account_storage(
    root: PathBuf,
    account: uuid::Uuid,
    credentials: Option<Arc<Credentials>>,
) -> Result<(), String> {
    let [services, _] = account_directories(&root, account);
    let matrix_store = tokio::fs::symlink_metadata(services.join("sdk"))
        .await
        .is_ok();
    erase_account_files(root, account).await?;
    if matrix_store && let Some(credentials) = credentials {
        // The key protects nothing once the store files are gone.
        let _ = credentials.delete_matrix_store_key(account).await;
    }
    Ok(())
}

async fn erase_account_files(root: PathBuf, account: uuid::Uuid) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        for directory in account_directories(&root, account) {
            match std::fs::symlink_metadata(&directory) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return Err("Removed account data could not be inspected.".to_owned()),
                Ok(metadata) if metadata.is_dir() => {
                    // `remove_dir_all` never follows symbolic links inside the tree.
                    std::fs::remove_dir_all(&directory)
                }
                Ok(_) => std::fs::remove_file(&directory),
            }
            .or_else(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    Ok(())
                } else {
                    Err("Removed account data could not be erased.".to_owned())
                }
            })?;
            if std::fs::symlink_metadata(&directory).is_ok() {
                return Err("Removed account data could not be erased.".to_owned());
            }
        }
        Ok(())
    })
    .await
    .map_err(|_| "Removed account data could not be erased.".to_owned())?
}

/// The key of a managed Matrix account's store, kept in the system keyring.
///
/// A legacy plaintext `database-key` file is moved into the keyring and removed
/// only after the keyring returns the same key.
async fn matrix_store_key(
    credentials: &Credentials,
    account: uuid::Uuid,
    directory: &Path,
) -> Result<Secret, String> {
    let keyring = |issue: CredentialIssue| {
        format!(
            "{} Matrix keeps its encryption key in the system keyring; unlock it and connect again.",
            issue.summary
        )
    };
    let legacy = directory.join("database-key");
    match tokio::fs::read_to_string(&legacy).await {
        Ok(key) => {
            let key = Secret::from(key);
            credentials
                .set_matrix_store_key(account, &key)
                .await
                .map_err(keyring)?;
            match credentials
                .matrix_store_key(account)
                .await
                .map_err(keyring)?
            {
                Some(stored) if stored.expose() == key.expose() => {}
                _ => return Err("The system keyring did not keep the Matrix key.".into()),
            }
            tokio::fs::remove_file(&legacy)
                .await
                .map_err(|_| "Could not update private service storage.")?;
            Ok(key)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if let Some(key) = credentials
                .matrix_store_key(account)
                .await
                .map_err(keyring)?
            {
                return Ok(key);
            }
            if tokio::fs::symlink_metadata(directory.join("sdk"))
                .await
                .is_ok()
            {
                return Err(
                    "The Matrix encryption key is missing from the system keyring. Remove and add this account again."
                        .into(),
                );
            }
            let key = Secret::from(format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            ));
            credentials
                .set_matrix_store_key(account, &key)
                .await
                .map_err(keyring)?;
            Ok(key)
        }
        Err(_) => Err("Could not read private service storage.".into()),
    }
}

/// Whether the socket at `path` accepts connections and was bound by process `pid`.
async fn served_by(path: &Path, pid: u32) -> bool {
    let Ok(socket) = LocalSocket::open(path) else {
        return false;
    };
    socket
        .connect()
        .await
        .ok()
        .and_then(|stream| stream.peer_cred().ok())
        .and_then(|credentials| credentials.pid())
        .is_some_and(|peer| u32::try_from(peer) == Ok(pid))
}

async fn private_directory(directory: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::create_dir_all(directory)
        .await
        .map_err(|_| "Could not create private service storage.")?;
    tokio::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
        .await
        .map_err(|_| "Could not protect service storage.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    #[tokio::test]
    async fn services_signal_starts_privately_and_stops_before_reopening_storage() {
        let directory = tempfile::tempdir().unwrap();
        let config = ConnectionConfig {
            account: uuid::Uuid::new_v4(),
            protocol: messages_core::ProtocolId::new("signal"),
            label: "Signal".into(),
            endpoint: String::new(),
            user_id: String::new(),
            rooms: Vec::new(),
            bot: false,
            native_bridge: false,
            managed: Some(ManagedConnection::Signal),
        };
        let program = directory.path().join("signal-fixture");
        tokio::fs::write(
            &program,
            r#"#!/usr/bin/env python3
import json, os, socket, sys
assert '--receive-mode=manual' in sys.argv
assert '--no-receive-stdout' in sys.argv
path = next(arg.split('=', 1)[1] for arg in sys.argv if arg.startswith('--socket='))
server = socket.socket(socket.AF_UNIX)
server.bind(path)
server.listen()
while True:
    connection, _ = server.accept()
    with connection, connection.makefile('rwb') as stream:
        for line in stream:
            request = json.loads(line)
            stream.write(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': {'version': 'fixture'}}).encode() + b'\n')
            stream.flush()
"#,
        )
        .await
        .unwrap();
        tokio::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700))
            .await
            .unwrap();
        let prepared = prepare_with_programs(
            config,
            directory.path().to_owned(),
            None,
            directory.path().to_owned(),
            Some(program),
        )
        .await
        .unwrap();
        assert!(prepared.token.is_none());
        let socket = LocalSocket::for_config(&prepared.config).unwrap().unwrap();
        let mut stream = BufReader::new(socket.connect().await.unwrap());
        stream
            .get_mut()
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":\"1\",\"method\":\"version\"}\n")
            .await
            .unwrap();
        let mut response = String::new();
        stream.read_line(&mut response).await.unwrap();
        assert!(response.contains("\"version\": \"fixture\""));
        drop(stream);
        prepared.host.stop().await;
        assert!(prepared.host.child.lock().unwrap().is_none());
        assert!(socket.connect().await.is_err());
        for private in [
            directory.path().join("services"),
            directory
                .path()
                .join("services")
                .join(prepared.config.account.to_string()),
        ] {
            assert_eq!(
                std::fs::metadata(private).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
    }

    #[tokio::test]
    async fn services_matrix_key_moves_into_the_keyring_only_when_it_is_kept() {
        use enzui_services::credential_store::{
            MemoryCredentialStore, UnlockCancelledCredentialStore,
        };
        let directory = tempfile::tempdir().unwrap();
        let account = uuid::Uuid::new_v4();
        let legacy = directory.path().join("database-key");
        std::fs::write(&legacy, "legacy-store-key-0123456789abcdef").unwrap();

        let locked = Credentials::with_store(Arc::new(UnlockCancelledCredentialStore));
        assert!(
            matrix_store_key(&locked, account, directory.path())
                .await
                .is_err()
        );
        assert!(legacy.exists());

        let keyring = Credentials::with_store(Arc::new(MemoryCredentialStore::new()));
        let key = matrix_store_key(&keyring, account, directory.path())
            .await
            .unwrap();
        assert_eq!(key.expose(), "legacy-store-key-0123456789abcdef");
        assert!(!legacy.exists());
        let again = matrix_store_key(&keyring, account, directory.path())
            .await
            .unwrap();
        assert_eq!(again.expose(), key.expose());

        let other = uuid::Uuid::new_v4();
        std::fs::create_dir(directory.path().join("sdk")).unwrap();
        assert!(
            matrix_store_key(&keyring, other, directory.path())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn services_readiness_accepts_only_the_socket_of_the_started_process() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(SOCKET_NAME);
        let _listener = tokio::net::UnixListener::bind(&path).unwrap();
        assert!(served_by(&path, std::process::id()).await);
        assert!(!served_by(&path, std::process::id() + 1).await);
        assert!(!served_by(&directory.path().join("missing.sock"), std::process::id()).await);
    }
}
