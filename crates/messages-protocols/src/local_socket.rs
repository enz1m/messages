//! Private Unix sockets of the helpers that Messages starts for managed accounts.

use std::{
    ffi::OsString,
    os::fd::{AsRawFd, OwnedFd},
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::{AdapterError, ConnectionConfig};

const SCHEME: &str = "unix:";

/// A helper socket inside a private directory.
///
/// The directory stays open for the lifetime of the value and connections go
/// through `/proc/self/fd`, so deep state directories stay within the 108-byte
/// socket path limit.
#[derive(Clone, Debug)]
pub struct LocalSocket {
    directory: Arc<OwnedFd>,
    name: OsString,
}

impl LocalSocket {
    /// The connection endpoint for the socket at `path`, or `None` when the path is not UTF-8.
    pub fn endpoint(path: &Path) -> Option<String> {
        path.to_str().map(|path| format!("{SCHEME}{path}"))
    }

    /// The socket named by a managed connection's endpoint.
    ///
    /// Returns `None` for network endpoints. Only managed connections may name a socket.
    pub fn for_config(config: &ConnectionConfig) -> Option<Result<Self, AdapterError>> {
        let path = config.endpoint.strip_prefix(SCHEME)?;
        Some(if config.managed.is_some() {
            Self::open(Path::new(path)).map_err(|_| AdapterError::Network)
        } else {
            Err(AdapterError::Configuration(
                "Use an HTTP(S) server URL.".into(),
            ))
        })
    }

    /// Opens the directory holding the socket at `path`.
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let invalid = || std::io::Error::from(std::io::ErrorKind::InvalidInput);
        let directory = path
            .parent()
            .filter(|parent| parent.is_absolute())
            .ok_or_else(invalid)?;
        let name = path.file_name().ok_or_else(invalid)?;
        let directory = std::fs::File::open(directory)?;
        if !directory.metadata()?.is_dir() {
            return Err(invalid());
        }
        Ok(Self {
            directory: Arc::new(directory.into()),
            name: name.to_owned(),
        })
    }

    /// A short path that reaches the socket through the held directory.
    pub fn path(&self) -> PathBuf {
        Path::new("/proc/self/fd")
            .join(self.directory.as_raw_fd().to_string())
            .join(&self.name)
    }

    pub async fn connect(&self) -> std::io::Result<tokio::net::UnixStream> {
        tokio::net::UnixStream::connect(self.path()).await
    }
}
