use super::*;
use crate::{Backend, Command};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    response::IntoResponse,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

const ROOM: &str = "!keys:example.test";
const OWN: &str = "@you:example.test";

async fn account(directory: &std::path::Path, server: &str, device: &str) -> Arc<Matrix> {
    std::fs::write(
        directory.join("session.json"),
        serde_json::to_vec(&json!({
            "user_id":OWN,"device_id":device,"access_token":"synthetic-local-token"
        }))
        .unwrap(),
    )
    .unwrap();
    Matrix::prepare(directory, server).await.unwrap()
}

#[tokio::test]
async fn late_room_keys_replace_persisted_placeholders_without_resurrecting_redactions() {
    let ciphertext = Arc::new(Mutex::new(Vec::<Value>::new()));
    let wire = ciphertext.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server = format!("http://{}", listener.local_addr().unwrap());
    let router = Router::new().fallback(move |request: Request<Body>| {
        let wire = wire.clone();
        async move {
            let path = request.uri().path().trim_end_matches('/').to_owned();
            let state = json!([
                {"type":"m.room.encryption","state_key":"","event_id":"$encryption:example.test","sender":OWN,"origin_server_ts":1000,"content":{"algorithm":"m.megolm.v1.aes-sha2"}},
                {"type":"m.room.member","state_key":OWN,"event_id":"$member:example.test","sender":OWN,"origin_server_ts":1000,"content":{"membership":"join","displayname":"Synthetic"}}
            ]);
            if path.ends_with("/versions") { return axum::Json(json!({"versions":["v1.11"]})).into_response(); }
            if path.ends_with("/createRoom") { return axum::Json(json!({"room_id":ROOM})).into_response(); }
            if path.ends_with("/account_data/m.direct") { return axum::Json(json!({})).into_response(); }
            if path.ends_with("/state/m.room.encryption") { return axum::Json(state[0]["content"].clone()).into_response(); }
            if path.ends_with("/state") { return axum::Json(state).into_response(); }
            if path.ends_with("/members") { return axum::Json(json!({"chunk":[state[1]]})).into_response(); }
            if path.ends_with("/keys/upload") { return axum::Json(json!({"one_time_key_counts":{"signed_curve25519":50}})).into_response(); }
            if path.ends_with("/keys/query") { return axum::Json(json!({"device_keys":{},"failures":{}})).into_response(); }
            if path.ends_with("/keys/claim") { return axum::Json(json!({"one_time_keys":{}})).into_response(); }
            if path.contains("/sendToDevice/") { return axum::Json(json!({})).into_response(); }
            if path.contains("/send/") {
                assert!(path.contains("/m.room.encrypted/"));
                let content: Value = serde_json::from_slice(&to_bytes(request.into_body(), 1024*1024).await.unwrap()).unwrap();
                let mut wire = wire.lock().unwrap();
                let id = format!("$encrypted-{}:example.test", wire.len());
                let timestamp = 2000 + wire.len();
                wire.push(json!({"type":"m.room.encrypted","event_id":id,"sender":OWN,"origin_server_ts":timestamp,"content":content}));
                return axum::Json(json!({"event_id":id})).into_response();
            }
            if path.ends_with("/messages") { return axum::Json(json!({"start":"page0","chunk":wire.lock().unwrap().clone(),"state":[]})).into_response(); }
            if let Some(id) = path.split("/event/").nth(1) {
                let id = percent_encoding::percent_decode_str(id).decode_utf8().unwrap();
                if let Some(event) = wire.lock().unwrap().iter().find(|event|event["event_id"]==id.as_ref()) {
                    return axum::Json(event.clone()).into_response();
                }
            }
            (StatusCode::NOT_FOUND, axum::Json(json!({"errcode":"M_NOT_FOUND","error":"local fixture unavailable"}))).into_response()
        }
    });
    let authority = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let sender_directory = tempfile::tempdir().unwrap();
    let sender = account(sender_directory.path(), &server, "SENDER").await;
    sender
        .command(
            "open",
            serde_json::from_value(json!({"chat_id":"@peer:example.test"})).unwrap(),
        )
        .await
        .unwrap();
    for body in [
        "History unlocked",
        "Redacted secret",
        "Older cache unlocked",
    ] {
        sender
            .command(
                "send",
                serde_json::from_value(
                    json!({"chat_id":ROOM,"body":body,"transaction_id":uuid::Uuid::new_v4()}),
                )
                .unwrap(),
            )
            .await
            .unwrap();
    }
    let export = sender_directory.path().join("keys");
    sender
        .client
        .encryption()
        .export_room_keys(export.clone(), "synthetic-export-password", |_| true)
        .await
        .unwrap();

    let receiver_directory = tempfile::tempdir().unwrap();
    let receiver = account(receiver_directory.path(), &server, "RECEIVER").await;
    receiver
        .command(
            "open",
            serde_json::from_value(json!({"chat_id":"@peer:example.test"})).unwrap(),
        )
        .await
        .unwrap();
    let history = || serde_json::from_value::<Command>(json!({"chat_id":ROOM})).unwrap();
    receiver.command("history", history()).await.unwrap();
    let key = RoomKey::new(uuid::Uuid::nil(), ROOM);
    {
        let mut cache = receiver.cache.lock().await;
        assert_eq!(cache.matrix.pending_decryption().len(), 3);
        assert!(
            cache
                .store
                .room(&key)
                .unwrap()
                .messages
                .iter()
                .all(|m| m.waiting_for_keys())
        );
        cache.store.room_mut(&key).unwrap().draft = "Unsent local draft".into();
        cache.apply_matrix(&key, &json!({"event_id":"$redaction:example.test","type":"m.room.redaction","content":{"redacts":"$encrypted-1:example.test"}}), OWN).unwrap();
        cache.persist().await.unwrap();
    }
    assert_eq!(
        receiver
            .cache
            .lock()
            .await
            .matrix
            .pending_decryption()
            .len(),
        2
    );
    drop(receiver);
    let path = receiver_directory.path().join("messages.json");
    let mut old: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    old["matrix"]["rooms"][ROOM]["messages"]["$encrypted-2:example.test"]
        .as_object_mut()
        .unwrap()
        .remove("ciphertext");
    for message in old["conversations"][0]["messages"].as_array_mut().unwrap() {
        message.as_object_mut().unwrap().remove("content_state");
    }
    std::fs::write(path, serde_json::to_vec(&old).unwrap()).unwrap();
    let receiver = Matrix::prepare(receiver_directory.path(), &server)
        .await
        .unwrap();
    assert_eq!(
        receiver
            .cache
            .lock()
            .await
            .store
            .room(&key)
            .unwrap()
            .messages
            .iter()
            .filter(|message| message.waiting_for_keys())
            .count(),
        2
    );
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let worker_receiver = receiver.clone();
    let worker = tokio::spawn(async move { worker_receiver.receive_keys(stopped).await });
    let imported = receiver
        .client
        .encryption()
        .import_room_keys(export, "synthetic-export-password")
        .await
        .unwrap();
    assert_eq!(imported.imported_count, 1);
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let cache = receiver.cache.lock().await;
            if cache.store.room(&key).unwrap().messages[0].body == "History unlocked"
                && cache.matrix.pending_decryption().is_empty()
            {
                break;
            }
            drop(cache);
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    stop.send(true).unwrap();
    worker.await.unwrap().unwrap();
    {
        let cache = receiver.cache.lock().await;
        let room = cache.store.room(&key).unwrap();
        assert_eq!(room.messages.len(), 3);
        assert!(room.messages[1].deleted && room.messages[1].body.is_empty());
        assert_eq!(room.messages[2].body, "Older cache unlocked");
        assert_eq!(room.draft, "Unsent local draft");
        assert!(cache.matrix.pending_decryption().is_empty());
    }
    drop(receiver);
    let receiver = Matrix::prepare(receiver_directory.path(), &server)
        .await
        .unwrap();
    receiver.command("history", history()).await.unwrap();
    let cache = receiver.cache.lock().await;
    assert_eq!(
        cache.store.room(&key).unwrap().messages[0].body,
        "History unlocked"
    );
    assert!(cache.store.room(&key).unwrap().messages[1].deleted);
    authority.abort();
}
