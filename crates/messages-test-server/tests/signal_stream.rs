use messages_core::{ProtocolId, fixture};
use messages_protocols::{ConnectionConfig, Registry};
use messages_test_server::TestServer;
use serde_json::json;

fn config(server: &TestServer) -> ConnectionConfig {
    ConnectionConfig {
        account: fixture::account_id(0),
        protocol: ProtocolId::new("signal"),
        label: "Signal".into(),
        endpoint: format!("{}/signal", server.url),
        user_id: "you".into(),
        rooms: Vec::new(),
        bot: false,
        native_bridge: false,
        managed: None,
    }
}

fn event(body: &str) -> String {
    let value = json!({"params":{"account":"you","envelope":{"source":"+15550100001","sourceName":"Maya",
        "dataMessage":{"timestamp":1791510100000_i64,"message":body}}}});
    format!("id: 101\r\nevent: receive\r\ndata: {value}\r\n\r\n")
}

#[tokio::test]
async fn signal_stream_retains_partial_events_and_connection_between_sync_windows() {
    let server = TestServer::start().await.unwrap();
    let text = "Café in 東京 — see you there 👋🏽";
    let bytes = event(text).into_bytes();
    let split = bytes.windows(3).position(|b| b == "東".as_bytes()).unwrap() + 1;
    server.state.lock().unwrap().signal_chunks = Some(
        bytes[..split]
            .chunks(1)
            .map(|b| (0, b.to_vec()))
            .chain(
                bytes[split..]
                    .chunks(1)
                    .enumerate()
                    .map(|(i, b)| (if i == 0 { 950 } else { 0 }, b.to_vec())),
            )
            .collect(),
    );
    let adapter = Registry::default().connect(config(&server), None).unwrap();
    let first = adapter.sync(None).await.unwrap();
    assert!(first.messages.is_empty());
    assert_ne!(first.cursor.as_deref(), Some("101"));
    let second = adapter.sync(first.cursor.as_deref()).await.unwrap();
    assert_eq!(second.messages.len(), 1);
    assert_eq!(second.messages[0].1[0].body, text);
    assert_eq!(second.cursor.as_deref(), Some("101"));
    assert_eq!(
        server
            .state
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|request| request.path.ends_with("/events"))
            .count(),
        1
    );
}

#[tokio::test]
async fn signal_actions_address_the_remote_identity_not_the_optimistic_clock() {
    use messages_protocols::MessageAction;
    let server = TestServer::start().await.unwrap();
    let adapter = Registry::default().connect(config(&server), None).unwrap();
    let store = fixture::sample_store();
    let room = &store.conversations[0];
    let mut own = room
        .messages
        .iter()
        .find(|message| message.own)
        .unwrap()
        .clone();
    let remote_timestamp = own.timestamp.timestamp_millis();
    own.id = format!("you:{remote_timestamp}");
    own.timestamp = chrono::Utc::now();
    adapter
        .action(
            &room.key,
            MessageAction::Edit {
                message: own.clone(),
                body: "Edited using the server identity".into(),
            },
        )
        .await
        .unwrap();
    adapter
        .action(
            &room.key,
            MessageAction::React {
                message: own.clone(),
                emoji: "👍".into(),
                remove: false,
            },
        )
        .await
        .unwrap();
    adapter
        .action(&room.key, MessageAction::Delete { message: own })
        .await
        .unwrap();
    adapter
        .action(
            &room.key,
            MessageAction::Read {
                message: format!("+15550100001:{remote_timestamp}"),
            },
        )
        .await
        .unwrap();
    let state = server.state.lock().unwrap();
    assert_eq!(state.requests.len(), 4);
    assert_eq!(
        state.requests[0].body["params"]["editTimestamp"],
        remote_timestamp
    );
    assert_eq!(
        state.requests[1].body["params"]["targetTimestamp"],
        remote_timestamp
    );
    assert_eq!(
        state.requests[2].body["params"]["targetTimestamp"],
        remote_timestamp
    );
    assert_eq!(
        state.requests[3].body["params"]["recipient"],
        "+15550100001"
    );
    assert_eq!(
        state.requests[3].body["params"]["targetTimestamp"],
        json!([remote_timestamp])
    );
    let target = state
        .store
        .room(&room.key)
        .unwrap()
        .messages
        .iter()
        .find(|message| message.timestamp.timestamp_millis() == remote_timestamp)
        .unwrap();
    assert_eq!(target.body, "Edited using the server identity");
    assert!(target.edited);
}
