#![cfg(feature = "matrix")]

use messages_bridge::{Backend, matrix::Matrix};

#[tokio::test]
async fn native_matrix_creates_encrypted_room_and_roundtrips_ciphertext_through_sdk() {
    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{Request, StatusCode},
        response::IntoResponse,
    };
    use serde_json::{Value, json};
    use std::sync::{Arc, Mutex};
    let directory = tempfile::tempdir().unwrap();
    let created = Arc::new(Mutex::new(Vec::<Value>::new()));
    let observed = created.clone();
    let sent = Arc::new(Mutex::new(Vec::<Value>::new()));
    let received = sent.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server = format!("http://{}", listener.local_addr().unwrap());
    let router = Router::new().fallback(move |request: Request<Body>| {
        let observed = observed.clone();
        let received = received.clone();
        async move {
            let path = request.uri().path().trim_end_matches('/').to_owned();
            if path == "/_matrix/client/versions" {
                return axum::Json(json!({"versions":["v1.11"],"unstable_features":{}})).into_response();
            }
            if path.ends_with("/createRoom") {
                let bytes = to_bytes(request.into_body(), 1024 * 1024).await.unwrap();
                observed.lock().unwrap().push(serde_json::from_slice(&bytes).unwrap());
                return axum::Json(json!({"room_id":"!new:example.test"})).into_response();
            }
            if path.ends_with("/account_data/m.direct") {
                return axum::Json(json!({})).into_response();
            }
            let encryption = json!({"type":"m.room.encryption","state_key":"","event_id":"$encryption:example.test",
                "sender":"@you:example.test","origin_server_ts":1791510100000_i64,"content":{"algorithm":"m.megolm.v1.aes-sha2"}});
            let member = json!({"type":"m.room.member","state_key":"@you:example.test","event_id":"$membership:example.test",
                "sender":"@you:example.test","origin_server_ts":1791510100000_i64,"content":{"membership":"join","displayname":"You"}});
            if path.ends_with("/state/m.room.encryption") { return axum::Json(encryption["content"].clone()).into_response(); }
            if path.ends_with("/state") { return axum::Json(json!([encryption, member])).into_response(); }
            if path.ends_with("/members") { return axum::Json(json!({"chunk":[member]})).into_response(); }
            if path.ends_with("/keys/upload") { return axum::Json(json!({"one_time_key_counts":{"signed_curve25519":50}})).into_response(); }
            if path.ends_with("/keys/query") { return axum::Json(json!({"device_keys":{},"failures":{}})).into_response(); }
            if path.ends_with("/keys/claim") { return axum::Json(json!({"one_time_keys":{},"failures":{}})).into_response(); }
            if path.contains("/sendToDevice/") { return axum::Json(json!({})).into_response(); }
            if path.contains("/send/") {
                let event_type = path.split("/send/").nth(1).unwrap().split('/').next().unwrap().to_owned();
                let bytes = to_bytes(request.into_body(), 1024 * 1024).await.unwrap();
                let content: Value = serde_json::from_slice(&bytes).unwrap();
                let mut received = received.lock().unwrap();
                let id = if received.is_empty() { "$sent:example.test".to_owned() } else { format!("$sent-{}:example.test", received.len() + 1) };
                received.push(json!({"type":event_type,"content":content,"event_id":id,
                    "sender":"@you:example.test","origin_server_ts":1791510100000_i64,"room_id":"!new:example.test"}));
                return axum::Json(json!({"event_id":id})).into_response();
            }
            if path.contains("/redact/") {
                let target = path.split("/redact/").nth(1).unwrap().split('/').next().unwrap();
                let target = percent_encoding::percent_decode_str(target).decode_utf8().unwrap();
                let mut received = received.lock().unwrap();
                let id = format!("$redacted-{}:example.test", received.len());
                received.push(json!({"type":"m.room.redaction","content":{"redacts":target},"redacts":target,"event_id":id,
                    "sender":"@you:example.test","origin_server_ts":1791510200000_i64,"room_id":"!new:example.test"}));
                return axum::Json(json!({"event_id":id})).into_response();
            }
            if path.ends_with("/messages") {
                return axum::Json(json!({"start":"page0","end":null,"chunk":received.lock().unwrap().clone(),"state":[]})).into_response();
            }
            (StatusCode::NOT_IMPLEMENTED, axum::Json(json!({"errcode":"M_UNRECOGNIZED","error":"fixture endpoint unavailable"}))).into_response()
        }
    });
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    std::fs::write(directory.path().join("session.json"), serde_json::to_vec(&json!({
        "user_id":"@you:example.test","device_id":"MESSAGES_FIXTURE","access_token":"fictional-matrix-token"
    })).unwrap()).unwrap();
    let backend = Matrix::prepare(directory.path(), &server).await.unwrap();
    let command = serde_json::from_value(json!({"chat_id":"@alex:example.test"})).unwrap();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        backend.command("open", command),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result["conversation"]["key"]["room"], "!new:example.test");
    assert_eq!(result["conversation"]["encryption"], "end_to_end");
    let text = "Encrypted fixture message — café 👋";
    let send = serde_json::from_value(
        json!({"chat_id":"!new:example.test","body":text,"transaction_id":uuid::Uuid::new_v4()}),
    )
    .unwrap();
    let ack = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        backend.command("send", send),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(ack["message_id"], "$sent:example.test");
    {
        let wire = sent.lock().unwrap();
        assert_eq!(wire.len(), 1);
        assert_eq!(wire[0]["type"], "m.room.encrypted");
        assert_eq!(wire[0]["content"]["algorithm"], "m.megolm.v1.aes-sha2");
        assert!(!serde_json::to_string(&*wire).unwrap().contains(text));
        assert!(
            wire[0]["content"]["ciphertext"]
                .as_str()
                .is_some_and(|ciphertext| !ciphertext.is_empty())
        );
    }
    {
        let mut cache = backend.cache().lock().await;
        cache
            .store
            .conversations
            .iter_mut()
            .for_each(|room| room.messages.clear());
        cache.matrix = messages_protocols::matrix_timeline::MatrixTimeline::default();
    }
    let history = serde_json::from_value(json!({"chat_id":"!new:example.test"})).unwrap();
    let page = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        backend.command("history", history),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        page["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["body"] == text)
    );
    let react =
        json!({"chat_id":"!new:example.test","message_id":"$sent:example.test","emoji":"👍"});
    backend
        .command("react", serde_json::from_value(react.clone()).unwrap())
        .await
        .unwrap();
    backend
        .command("react", serde_json::from_value(react.clone()).unwrap())
        .await
        .unwrap();
    for _ in 0..2 {
        backend
            .command(
                "history",
                serde_json::from_value(json!({"chat_id":"!new:example.test"})).unwrap(),
            )
            .await
            .unwrap();
        let cache = backend.cache().lock().await;
        let message = &cache.store.conversations[0].messages[0];
        assert_eq!(message.reactions.len(), 1);
        assert_eq!(message.reactions[0].count, 1);
        assert!(message.reactions[0].own);
    }
    assert_eq!(sent.lock().unwrap().len(), 2);
    let mut remove = react;
    remove["remove"] = json!(true);
    backend
        .command("react", serde_json::from_value(remove).unwrap())
        .await
        .unwrap();
    backend
        .command(
            "history",
            serde_json::from_value(json!({"chat_id":"!new:example.test"})).unwrap(),
        )
        .await
        .unwrap();
    assert!(
        backend.cache().lock().await.store.conversations[0].messages[0]
            .reactions
            .is_empty()
    );
    assert_eq!(
        sent.lock().unwrap()[2]["content"]["redacts"],
        "$sent-2:example.test"
    );
    let requests = created.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["invite"], json!(["@alex:example.test"]));
    assert_eq!(requests[0]["is_direct"], true);
    assert!(
        requests[0]["initial_state"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["type"] == "m.room.encryption"
                && event["content"]["algorithm"] == "m.megolm.v1.aes-sha2")
    );
    task.abort();
}
