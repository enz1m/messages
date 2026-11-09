//! Account-free HTTP fixtures at the adapters' real wire boundaries. These are
//! protocol-shaped test services, not substitutes for the networks' encryption.

mod bridge;
mod browser;
mod calls;
mod matrix;
mod signal;

use std::sync::{Arc, Mutex};

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use messages_core::{ChatMessage, Delivery, MessageStore, fixture};
use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub struct RecordedRequest {
    pub query: String,
    pub last_event: Option<String>,
    pub method: String,
    pub path: String,
    pub body: Value,
}

pub struct MockState {
    pub calls_enabled: bool,
    pub call_states:
        std::collections::HashMap<uuid::Uuid, (String, messages_protocols::calls::CallSnapshot)>,
    pub call_preview: Option<String>,
    pub login_steps: std::collections::BTreeMap<String, String>,
    pub avatar_data: std::collections::BTreeMap<String, Vec<u8>>,
    pub signal_account: Option<Value>,
    pub signal_link_failure: Option<(Option<Value>, Value)>,
    pub signal_attachment_data: Option<String>,
    pub signal_chunks: Option<Vec<(u64, Vec<u8>)>>,
    pub matrix_events: std::collections::BTreeMap<String, Vec<Value>>,
    pub store: MessageStore,
    pub requests: Vec<RecordedRequest>,
    pub next_failure: Option<u16>,
    pub browser_failure: Option<Value>,
    pub sequence: u64,
}

impl MockState {
    pub fn enable_calls(&mut self) {
        self.calls_enabled = true;
        let key = messages_core::RoomKey::new(fixture::account_id(3), "15550109999@s.whatsapp.net");
        if self.store.room(&key).is_none() {
            self.store
                .conversations
                .push(messages_core::Conversation::empty(
                    key,
                    "Synthetic call peer",
                    messages_core::RoomKind::Direct,
                    messages_core::Encryption::Sample,
                    fixture::sample_capabilities("whatsapp"),
                ));
        }
        for room in &mut self.store.conversations {
            if room.key.account == fixture::account_id(3)
                && room.kind == messages_core::RoomKind::Direct
            {
                room.capabilities = calls::capabilities(&room.capabilities);
            }
        }
    }
}

impl Default for MockState {
    fn default() -> Self {
        Self {
            calls_enabled: false,
            call_states: Default::default(),
            call_preview: None,
            login_steps: std::collections::BTreeMap::new(),
            avatar_data: Default::default(),
            signal_account: Some(json!({"number":"you","aci":"fixture-aci"})),
            signal_link_failure: None,
            signal_attachment_data: None,
            signal_chunks: None,
            matrix_events: std::collections::BTreeMap::new(),
            store: fixture::sample_store(),
            requests: Vec::new(),
            next_failure: None,
            browser_failure: None,
            sequence: 1000,
        }
    }
}

pub type SharedState = Arc<Mutex<MockState>>;

pub fn router(state: SharedState) -> Router {
    Router::new().fallback(handle).with_state(state)
}

pub struct TestServer {
    pub url: String,
    pub state: SharedState,
    task: tokio::task::JoinHandle<()>,
}

impl TestServer {
    pub async fn start() -> std::io::Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("http://{}", listener.local_addr()?);
        let state = Arc::new(Mutex::new(MockState::default()));
        let app = router(state.clone());
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self { url, state, task })
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn handle(State(shared): State<SharedState>, request: Request<Body>) -> Response {
    let method = request.method().to_string();
    let path = request.uri().path().to_owned();
    let query = request.uri().query().unwrap_or("").to_owned();
    let last_event = request
        .headers()
        .get("last-event-id")
        .and_then(|header| header.to_str().ok())
        .map(str::to_owned);
    let authorization = request
        .headers()
        .get("authorization")
        .and_then(|header| header.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let bytes = match axum::body::to_bytes(request.into_body(), 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(_) => return reply(413, json!({"error":"request too large"})),
    };
    let body: Value = if bytes.is_empty() {
        Value::Null
    } else {
        match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(_) => return reply(400, json!({"error":"invalid JSON"})),
        }
    };
    let segments: Vec<_> = path
        .trim_matches('/')
        .split('/')
        .map(|part| {
            percent_encoding::percent_decode_str(part)
                .decode_utf8_lossy()
                .into_owned()
        })
        .collect();
    let mut state = shared.lock().expect("mock state poisoned");
    if state.requests.len() >= 1000 {
        state.requests.remove(0);
    }
    let mut recorded_body = body.clone();
    if path.ends_with("/v1/login") && recorded_body.is_object() {
        recorded_body["value"] = json!("[redacted]");
    }
    state.requests.push(RecordedRequest {
        last_event: last_event.clone(),
        query: query.clone(),
        method: method.clone(),
        path,
        body: recorded_body,
    });
    if let Some(status) = state.next_failure.take() {
        return reply(status, json!({"error":"injected test failure"}));
    }
    let parts: Vec<&str> = segments.iter().map(String::as_str).collect();
    let Some((protocol, route)) = parts.split_first() else {
        return reply(404, json!({}));
    };
    if *protocol == "matrix"
        && !matches!(
            authorization.as_str(),
            "Bearer test-token" | "Bot test-token"
        )
    {
        return reply(401, json!({"error":"test token required"}));
    }
    if *protocol == "matrix" && route.first() == Some(&"v1") {
        return browser::handle(&mut state, &method, route, body);
    }
    match *protocol {
        "health" => reply(200, json!({"status":"ok","mode":"local-fixtures"})),
        "matrix" => matrix::handle(&mut state, &method, route, &query, body),
        "signal" => signal::handle(&mut state, &method, route, last_event.as_deref(), body),
        "whatsapp" => bridge::handle(&mut state, protocol, &method, route, &query, body),
        _ => reply(404, json!({"error":"unknown route"})),
    }
}

pub(crate) fn reply(status: u16, body: Value) -> Response {
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        axum::Json(body),
    )
        .into_response()
}

impl MockState {
    pub fn seed_history(&mut self, count: usize) {
        for room in &mut self.store.conversations {
            let Some(template) = room.messages.first().cloned() else {
                continue;
            };
            room.messages = (0..count.min(2000))
                .map(|index| {
                    let mut message = template.clone();
                    message.id = (100000 + index).to_string();
                    message.body = format!("History message {} — {}", index + 1, room.title);
                    message.timestamp =
                        Utc::now() - chrono::Duration::minutes((count - index) as i64);
                    message.reactions.clear();
                    message
                })
                .collect();
            room.has_older = false;
        }
    }
}

pub(crate) fn insert_message(
    state: &mut MockState,
    account: usize,
    room: &str,
    body: &str,
    transaction: Option<uuid::Uuid>,
    reply_to: Option<String>,
) -> Option<ChatMessage> {
    state.sequence += 1;
    let id = state.sequence.to_string();
    let key = messages_core::RoomKey::new(fixture::account_id(account), room);
    let sender = state
        .store
        .accounts
        .iter()
        .find(|account| account.id == key.account)?
        .user_id
        .clone();
    let message = ChatMessage {
        content_state: Some(messages_core::MessageContentState::Available),
        id,
        transaction,
        sender,
        sender_name: "You".into(),
        own: true,
        body: body.into(),
        timestamp: Utc::now(),
        delivery: Delivery::Sent,
        edited: false,
        deleted: false,
        reply_to,
        reactions: Vec::new(),
        attachments: Vec::new(),
    };
    state.store.room_mut(&key)?.messages.push(message.clone());
    Some(message)
}
