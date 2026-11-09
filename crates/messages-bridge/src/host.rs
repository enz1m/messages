use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use messages_core::Capability;
use serde_json::{Value, json};

use crate::{Backend, Command};

#[derive(Clone)]
struct Host {
    backend: Arc<dyn Backend>,
    token: Arc<String>,
}

pub async fn private_directory(protocol: &str) -> anyhow::Result<PathBuf> {
    let directory = if let Some(path) = std::env::var_os("MESSAGES_BRIDGE_STATE") {
        PathBuf::from(path)
    } else {
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share")
            });
        base.join("enz-messages/bridges").join(protocol)
    };
    tokio::fs::create_dir_all(&directory).await?;
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).await?;
    Ok(directory)
}

pub async fn token(directory: &std::path::Path) -> anyhow::Result<String> {
    if let Ok(value) = std::env::var("MESSAGES_BRIDGE_TOKEN") {
        anyhow::ensure!(
            value.len() >= 16,
            "Bridge token must contain at least 16 characters"
        );
        return Ok(value);
    }
    let path = directory.join("access-token");
    match tokio::fs::read_to_string(&path).await {
        Ok(value) => {
            anyhow::ensure!(value.len() >= 16, "Stored bridge token is invalid");
            Ok(value)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            use tokio::io::AsyncWriteExt;
            let value = format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            );
            let mut file = tokio::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(path)
                .await?;
            file.write_all(value.as_bytes()).await?;
            file.sync_all().await?;
            Ok(value)
        }
        Err(error) => Err(error.into()),
    }
}

pub fn router(backend: Arc<dyn Backend>, token: String) -> Router {
    let host = Host {
        backend,
        token: Arc::new(token),
    };
    Router::new()
        .route("/v1/status", get(status))
        .route(
            "/v1/browser",
            post(browser_start)
                .delete(browser_cancel)
                .layer(DefaultBodyLimit::max(16384)),
        )
        .route(
            "/v1/call",
            post(call_control).layer(DefaultBodyLimit::max(16384)),
        )
        .route("/v1/chats", get(chats))
        .route("/v1/avatar", get(avatar))
        .route(
            "/v1/login",
            get(login)
                .post(authenticate)
                .layer(DefaultBodyLimit::max(1024 * 1024)),
        )
        .route("/v1/{operation}", post(command))
        .layer(DefaultBodyLimit::max(140 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(host.clone(), authorize))
        .with_state(host)
}

async fn authorize(State(host): State<Host>, request: Request, next: Next) -> Response {
    let valid = request
        .headers()
        .get("authorization")
        .and_then(|header| header.to_str().ok())
        .and_then(|header| header.strip_prefix("Bearer "))
        .is_some_and(|value| value == *host.token);
    if !valid {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    next.run(request).await
}

async fn status(State(host): State<Host>) -> Json<messages_protocols::bridge::BridgeStatus> {
    Json(host.backend.status().await)
}

async fn chats(State(host): State<Host>) -> Response {
    if !host.backend.status().await.authenticated {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let calls = host.backend.calls().await;
    let cache = host.backend.cache().lock().await;
    let read_through: Vec<_> = cache
        .store
        .chat_states
        .iter()
        .map(|s| (&s.key, s.read_through))
        .collect();
    let typing: Vec<_> = cache
        .typing
        .values()
        .filter(|(_, time)| time.elapsed().as_secs() < 8)
        .map(|(typing, _)| typing)
        .collect();
    Json(json!({"conversations":cache.store.conversations, "read_through":read_through, "typing":typing,"calls":calls})).into_response()
}

async fn login(State(host): State<Host>) -> Json<Value> {
    Json(host.backend.login().await)
}

async fn authenticate(State(host): State<Host>, Json(input): Json<Value>) -> Response {
    match host.backend.authenticate(input).await {
        Ok(()) => Json(json!({"ok":true})).into_response(),
        Err(_) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"Authentication could not be completed."})),
        )
            .into_response(),
    }
}

async fn command(
    State(host): State<Host>,
    Path(operation): Path<String>,
    Json(command): Json<Command>,
) -> Response {
    let status = host.backend.status().await;
    if !status.authenticated {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let capability = match operation.as_str() {
        "open" => Some(Capability::StartConversation),
        "thread" => Some(Capability::Threads),
        "history" | "attachment" => None,
        "send" => Some(Capability::SendText),
        "react" => Some(Capability::React),
        "edit" => Some(Capability::EditOwn),
        "delete" => Some(Capability::DeleteOwn),
        "read" => Some(Capability::ReadReceipts),
        "typing" => Some(Capability::Typing),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    if capability.is_some_and(|capability| !status.capabilities.supports(capability)) {
        return (
            StatusCode::NOT_IMPLEMENTED,
            Json(json!({"error":"Unsupported action"})),
        )
            .into_response();
    }
    if messages_protocols::media::validate_total(
        command.attachments.len(),
        command.attachments.iter().map(|file| file.bytes),
    )
    .is_err()
        || command.attachments.iter().any(|file| {
            file.name.len() > 255
                || file.mime.len() > 128
                || file.data.len() > (file.bytes as usize).div_ceil(3) * 4
        })
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    if !command.attachments.is_empty()
        && (operation != "send" || !status.capabilities.supports(Capability::AttachFiles))
    {
        return StatusCode::NOT_IMPLEMENTED.into_response();
    }
    if command.body.len() > messages_core::store::MAX_MESSAGE_BYTES || command.chat_id.is_empty() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    if operation != "open" {
        let cache = host.backend.cache().lock().await;
        let key = messages_core::RoomKey::new(uuid::Uuid::nil(), &command.chat_id);
        let Some(room) = cache.store.room(&key) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        if (!command.attachments.is_empty() && !room.capabilities.supports(Capability::AttachFiles))
            || capability.is_some_and(|capability| !room.capabilities.supports(capability))
            || (operation == "send"
                && command.reply_to.is_some()
                && (!room.capabilities.supports(Capability::Reply)
                    || !status.capabilities.supports(Capability::Reply)))
        {
            return (
                StatusCode::NOT_IMPLEMENTED,
                Json(json!({"error":"Unsupported action in this conversation"})),
            )
                .into_response();
        }
    }
    match host.backend.command(&operation, command).await {
        Ok(result) => Json(result).into_response(),
        Err(_) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error":"The native protocol operation failed."})),
        )
            .into_response(),
    }
}

pub async fn serve(backend: Arc<dyn Backend>, token: String, port: u16) -> anyhow::Result<()> {
    let address: SocketAddr = std::env::var("MESSAGES_BRIDGE_BIND")
        .unwrap_or_else(|_| format!("127.0.0.1:{port}"))
        .parse()?;
    anyhow::ensure!(
        address.ip().is_loopback(),
        "The local bridge must bind to a loopback address"
    );
    let listener = tokio::net::TcpListener::bind(address).await?;
    println!(
        "Messages native bridge listening at http://{}",
        listener.local_addr()?
    );
    axum::serve(listener, router(backend, token))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

async fn call_control(
    State(host): State<Host>,
    Json(request): Json<messages_protocols::calls::CallRequest>,
) -> Response {
    use messages_protocols::calls::CallCommand;
    if request.chat_id.is_empty() || request.chat_id.len() > 512 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    if !matches!(request.command, CallCommand::End { .. }) {
        let status = host.backend.status().await;
        if !status.authenticated {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        let capability = match &request.command {
            CallCommand::Start { kind, .. } => kind.capability(),
            CallCommand::Camera { .. } => Capability::VideoCall,
            _ => Capability::VoiceCall,
        };
        let cache = host.backend.cache().lock().await;
        let key = messages_core::RoomKey::new(uuid::Uuid::nil(), &request.chat_id);
        if !status.capabilities.supports(capability)
            || !cache.store.room(&key).is_some_and(|r| {
                r.kind == messages_core::RoomKind::Direct && r.capabilities.supports(capability)
            })
        {
            return StatusCode::NOT_IMPLEMENTED.into_response();
        }
    }
    match host.backend.call_control(request).await {
        Ok(value) => Json(value).into_response(),
        Err(_) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"The call could not be completed. Check devices and connection."})),
        )
            .into_response(),
    }
}

#[derive(serde::Deserialize)]
struct BrowserRequest {
    request_id: uuid::Uuid,
}
async fn browser_start(State(host): State<Host>, Json(request): Json<BrowserRequest>) -> Response {
    match host.backend.browser_start(request.request_id).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => {
            let failure = error
                .downcast_ref::<messages_protocols::login::BrowserFailure>()
                .copied()
                .unwrap_or(messages_protocols::login::BrowserFailure::StartFailed);
            (StatusCode::BAD_REQUEST, Json(json!({"error_code":failure}))).into_response()
        }
    }
}
async fn browser_cancel(State(host): State<Host>, Json(request): Json<BrowserRequest>) -> Response {
    match host.backend.browser_cancel(request.request_id).await {
        Ok(()) => Json(json!({"ok":true})).into_response(),
        Err(_) => StatusCode::BAD_REQUEST.into_response(),
    }
}

#[derive(serde::Deserialize)]
struct AvatarQuery {
    chat_id: String,
}
async fn avatar(State(host): State<Host>, Query(query): Query<AvatarQuery>) -> Response {
    if !host.backend.status().await.authenticated {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if query.chat_id.len() > 512
        || !host
            .backend
            .cache()
            .lock()
            .await
            .store
            .conversations
            .iter()
            .any(|r| r.key.room == query.chat_id)
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    match host.backend.avatar(&query.chat_id).await {
        Ok(Some(bytes)) if bytes.len() <= 262144 => (
            [
                ("content-type", "application/octet-stream"),
                ("cache-control", "no-store"),
            ],
            bytes,
        )
            .into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        _ => StatusCode::BAD_GATEWAY.into_response(),
    }
}
