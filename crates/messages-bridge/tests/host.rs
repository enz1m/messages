use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use messages_bridge::{
    Backend, Command,
    cache::{Cache, SharedCache},
};
use messages_core::{Capabilities, Capability};
use messages_protocols::bridge::BridgeStatus;
use serde_json::{Value, json};

struct Probe {
    cache: SharedCache,
    calls: AtomicUsize,
    photos: AtomicUsize,
}

#[async_trait]
impl Backend for Probe {
    async fn avatar(&self, _chat: &str) -> anyhow::Result<Option<Vec<u8>>> {
        self.photos.fetch_add(1, Ordering::SeqCst);
        Ok(Some(vec![1, 2, 3]))
    }
    async fn status(&self) -> BridgeStatus {
        BridgeStatus {
            api_version: 1,
            protocol: "whatsapp".into(),
            authenticated: true,
            user_id: Some("fictional-user".into()),
            capabilities: Capabilities::new([Capability::SendText]),
            login_state: None,
        }
    }
    async fn login(&self) -> Value {
        json!({"state":"qr","qr":"fictional-pairing-secret"})
    }
    async fn authenticate(&self, _: Value) -> anyhow::Result<()> {
        Ok(())
    }
    fn cache(&self) -> &SharedCache {
        &self.cache
    }
    async fn command(&self, _: &str, _: Command) -> anyhow::Result<Value> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"message_id":"remote-1"}))
    }
}

async fn host() -> (
    tempfile::TempDir,
    Arc<Probe>,
    String,
    tokio::task::JoinHandle<()>,
) {
    let directory = tempfile::tempdir().unwrap();
    let cache = Cache::open(directory.path().join("history.json"))
        .await
        .unwrap();
    let probe = Arc::new(Probe {
        cache,
        calls: AtomicUsize::new(0),
        photos: AtomicUsize::new(0),
    });
    probe
        .cache
        .lock()
        .await
        .store
        .conversations
        .push(messages_core::Conversation::empty(
            messages_core::RoomKey::new(uuid::Uuid::nil(), "chat"),
            "Chat",
            messages_core::RoomKind::Direct,
            messages_core::Encryption::EndToEnd,
            Capabilities::new([Capability::SendText, Capability::Reply]),
        ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let app = messages_bridge::host::router(probe.clone(), "test-bridge-token".into());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (directory, probe, address, task)
}

#[tokio::test]
async fn host_authentication_protects_pairing_data_and_commands() {
    let (_directory, probe, address, task) = host().await;
    let client = reqwest::Client::new();
    for path in ["status", "login", "chats", "avatar?chat_id=chat"] {
        let response = client
            .get(format!("{address}/v1/{path}"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 401);
        assert!(
            !response
                .text()
                .await
                .unwrap()
                .contains("fictional-pairing-secret")
        );
    }
    for method in [reqwest::Method::POST, reqwest::Method::DELETE] {
        let response = client
            .request(method, format!("{address}/v1/browser"))
            .json(&json!({"request_id":uuid::Uuid::new_v4()}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 401);
    }
    let oversized = client
        .post(format!("{address}/v1/browser"))
        .bearer_auth("test-bridge-token")
        .header("content-type", "application/json")
        .body("x".repeat(16385))
        .send()
        .await
        .unwrap();
    assert_eq!(oversized.status(), 413);
    let blocked = client
        .post(format!("{address}/v1/send"))
        .json(&json!({"chat_id":"chat","body":"hello"}))
        .send()
        .await
        .unwrap();
    assert_eq!(blocked.status(), 401);
    assert_eq!(probe.calls.load(Ordering::SeqCst), 0);
    let allowed = client
        .get(format!("{address}/v1/login"))
        .bearer_auth("test-bridge-token")
        .send()
        .await
        .unwrap();
    assert_eq!(allowed.status(), 200);
    assert_eq!(
        allowed.json::<Value>().await.unwrap()["qr"],
        "fictional-pairing-secret"
    );
    let unknown = client
        .get(format!("{address}/v1/avatar?chat_id=unknown"))
        .bearer_auth("test-bridge-token")
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), 404);
    assert_eq!(probe.photos.load(Ordering::SeqCst), 0);
    let photo = client
        .get(format!("{address}/v1/avatar?chat_id=chat"))
        .bearer_auth("test-bridge-token")
        .send()
        .await
        .unwrap();
    assert_eq!(photo.status(), 200);
    assert_eq!(photo.headers()["cache-control"], "no-store");
    assert_eq!(photo.bytes().await.unwrap().as_ref(), &[1, 2, 3]);
    assert_eq!(probe.photos.load(Ordering::SeqCst), 1);
    task.abort();
}

#[tokio::test]
async fn host_capabilities_and_input_bounds_stop_native_dispatch() {
    let (_directory, probe, address, task) = host().await;
    let client = reqwest::Client::new();
    let post = |operation: &str, body: Value| {
        client
            .post(format!("{address}/v1/{operation}"))
            .bearer_auth("test-bridge-token")
            .json(&body)
    };
    assert_eq!(
        post("react", json!({"chat_id":"chat","emoji":"👍"}))
            .send()
            .await
            .unwrap()
            .status(),
        501
    );
    assert_eq!(
        post("send", json!({"chat_id":"chat","body":"x".repeat(65537)}))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    assert_eq!(
        post("send", json!({"chat_id":"","body":"hello"}))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    assert_eq!(probe.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        post("send", json!({"chat_id":"chat","body":"hello"}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
    task.abort();
}

#[tokio::test]
async fn host_token_is_private_and_stable_across_restart() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let first = messages_bridge::host::token(directory.path())
        .await
        .unwrap();
    let second = messages_bridge::host::token(directory.path())
        .await
        .unwrap();
    assert_eq!(first, second);
    assert!(first.len() >= 32);
    assert_eq!(
        std::fs::metadata(directory.path().join("access-token"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[tokio::test]
async fn host_room_and_account_reply_restrictions_stop_the_backend() {
    let (_directory, probe, address, task) = host().await;
    let client = reqwest::Client::new();
    let send = |body: Value| {
        client
            .post(format!("{address}/v1/send"))
            .bearer_auth("test-bridge-token")
            .json(&body)
    };
    assert_eq!(
        send(json!({"chat_id":"chat","body":"Plain send"}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        send(json!({"chat_id":"chat","body":"Reply","reply_to":"parent"}))
            .send()
            .await
            .unwrap()
            .status(),
        501
    );
    probe.cache.lock().await.store.conversations[0]
        .capabilities
        .disable(Capability::SendText, "Read-only room");
    assert_eq!(
        send(json!({"chat_id":"chat","body":"Blocked send"}))
            .send()
            .await
            .unwrap()
            .status(),
        501
    );
    assert_eq!(
        send(json!({"chat_id":"unknown","body":"Unknown room"}))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
    task.abort();
}
