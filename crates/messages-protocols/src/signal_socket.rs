//! signal-cli JSON-RPC over its daemon socket: one JSON object per line.

use std::time::Duration;

use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

use crate::{AdapterError, local_socket::LocalSocket};

const MAX_NOTIFICATION_BYTES: usize = 1024 * 1024;

/// Sends one request on its own connection and returns the response with `id`.
///
/// The daemon runs with manual receive mode, so a request connection never
/// carries received messages that it could drop.
pub(crate) async fn call(
    socket: &LocalSocket,
    request: &Value,
    id: &str,
    limit: usize,
    timeout: Duration,
) -> Result<Value, AdapterError> {
    tokio::time::timeout(timeout, async {
        let mut stream = BufReader::new(socket.connect().await.map_err(|_| AdapterError::Network)?);
        write(stream.get_mut(), request).await?;
        let mut line = Vec::new();
        loop {
            line.clear();
            read_line(&mut stream, &mut line, limit).await?;
            if line.is_empty() {
                return Err(AdapterError::Network);
            }
            let value = parse(std::mem::take(&mut line)).await?;
            if value["id"] == id {
                return Ok(value);
            }
        }
    })
    .await
    .map_err(|_| AdapterError::Network)?
}

/// A connection subscribed to one account's received messages.
pub(crate) struct Subscription {
    stream: BufReader<UnixStream>,
    line: Vec<u8>,
}

impl Subscription {
    pub async fn open(socket: &LocalSocket, account: &str) -> Result<Self, AdapterError> {
        let id = uuid::Uuid::new_v4().to_string();
        let mut subscription = Self {
            stream: BufReader::new(socket.connect().await.map_err(|_| AdapterError::Network)?),
            line: Vec::new(),
        };
        let request = json!({
            "jsonrpc":"2.0","id":id,"method":"subscribeReceive","params":{"account":account}
        });
        write(subscription.stream.get_mut(), &request).await?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(35);
        loop {
            let Some(value) = subscription.next(deadline).await? else {
                return Err(AdapterError::Network);
            };
            if value["id"] != id {
                continue;
            }
            if !value["error"].is_null() {
                return Err(AdapterError::InvalidData(
                    "signal-cli rejected subscribeReceive".into(),
                ));
            }
            return Ok(subscription);
        }
    }

    /// The next message before `deadline`, or `None` when the deadline passes.
    ///
    /// A partial line is kept for the next call. A closed connection is an error.
    pub async fn next(
        &mut self,
        deadline: tokio::time::Instant,
    ) -> Result<Option<Value>, AdapterError> {
        let read = read_line(&mut self.stream, &mut self.line, MAX_NOTIFICATION_BYTES);
        match tokio::time::timeout_at(deadline, read).await {
            Err(_) => Ok(None),
            Ok(result) => {
                result?;
                if !self.line.ends_with(b"\n") {
                    return Err(AdapterError::Network);
                }
                parse(std::mem::take(&mut self.line)).await.map(Some)
            }
        }
    }
}

async fn write(stream: &mut UnixStream, request: &Value) -> Result<(), AdapterError> {
    let mut bytes = serde_json::to_vec(request).map_err(|_| AdapterError::Network)?;
    bytes.push(b'\n');
    stream
        .write_all(&bytes)
        .await
        .map_err(|_| AdapterError::Network)
}

/// Appends up to one line to `line`; cancellation keeps the bytes already read.
async fn read_line(
    stream: &mut BufReader<UnixStream>,
    line: &mut Vec<u8>,
    limit: usize,
) -> Result<(), AdapterError> {
    let remaining = (limit + 1).saturating_sub(line.len()) as u64;
    stream
        .take(remaining)
        .read_until(b'\n', line)
        .await
        .map_err(|_| AdapterError::Network)?;
    if line.len() > limit {
        return Err(AdapterError::InvalidData(
            "response exceeds the size limit".into(),
        ));
    }
    Ok(())
}

async fn parse(line: Vec<u8>) -> Result<Value, AdapterError> {
    let invalid = |_| AdapterError::InvalidData("invalid JSON".into());
    if line.len() > 1024 * 1024 {
        tokio::task::spawn_blocking(move || serde_json::from_slice(&line))
            .await
            .map_err(|_| AdapterError::Network)?
            .map_err(invalid)
    } else {
        serde_json::from_slice(&line).map_err(invalid)
    }
}
