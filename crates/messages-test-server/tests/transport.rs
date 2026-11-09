use messages_core::{ProtocolId, RoomKey, SendRequest, fixture};
use messages_protocols::{AdapterError, ConnectionConfig, Registry};
use messages_test_server::TestServer;
use uuid::Uuid;

fn config(server: &TestServer, protocol: &str, index: usize) -> ConnectionConfig {
    ConnectionConfig {
        account: fixture::account_id(index),
        protocol: ProtocolId::new(protocol),
        label: protocol.into(),
        endpoint: format!("{}/{protocol}", server.url),
        user_id: if protocol == "matrix" {
            "@you:example.test".into()
        } else {
            "you".into()
        },
        rooms: Vec::new(),
        bot: false,
        native_bridge: false,
        managed: None,
    }
}

#[tokio::test]
async fn transport_adapters_read_send_and_report_typed_http_failures() {
    {
        let server = TestServer::start().await.unwrap();
        for (index, (protocol, _)) in fixture::PROTOCOLS.into_iter().enumerate() {
            let index = if protocol == "whatsapp" { 3 } else { index };
            let adapter = Registry::default()
                .connect(config(&server, protocol, index), Some("test-token".into()))
                .unwrap();
            let batch = adapter.sync(None).await.unwrap();
            assert!(
                !batch.conversations.is_empty(),
                "{protocol} did not load conversations"
            );
            let key = batch.conversations[0].key.clone();
            let text = format!("Message sent through the {protocol} adapter ✓");
            let request = SendRequest {
                transaction: Uuid::new_v4(),
                room: key.clone(),
                body: text.clone(),
                reply_to: None,
                attachments: Vec::new(),
            };
            let mut other_account = request.clone();
            other_account.room.account = Uuid::new_v4();
            let before = server.state.lock().unwrap().requests.len();
            assert!(matches!(
                adapter.send(&other_account).await,
                Err(AdapterError::Configuration(_))
            ));
            assert_eq!(server.state.lock().unwrap().requests.len(), before);
            let remote = adapter.send(&request).await.unwrap();
            assert!(!remote.is_empty());
            let state = server.state.lock().unwrap();
            let room = state.store.room(&key).unwrap();
            assert!(
                room.messages.iter().any(|message| message.body == text),
                "{protocol} write did not reach the server"
            );
            if protocol == "signal" {
                assert!(
                    !batch.messages.is_empty(),
                    "Signal must consume SSE, not just contacts"
                );
            } else {
                assert!(
                    !batch.conversations[0].messages.is_empty(),
                    "{protocol} history missing"
                );
            }
        }
    }

    {
        let server = TestServer::start().await.unwrap();
        let invalid = Registry::default()
            .connect(config(&server, "matrix", 1), Some("wrong-token".into()))
            .unwrap();
        assert!(matches!(
            invalid.sync(None).await,
            Err(AdapterError::Authentication)
        ));
        let valid = Registry::default()
            .connect(config(&server, "matrix", 1), Some("test-token".into()))
            .unwrap();
        server.state.lock().unwrap().next_failure = Some(429);
        assert!(matches!(
            valid.sync(None).await,
            Err(AdapterError::RateLimited(_))
        ));
    }
}

#[tokio::test]
async fn matrix_send_enforces_encryption_idempotence_and_reply_relations() {
    {
        let server = TestServer::start().await.unwrap();
        let adapter = Registry::default()
            .connect(config(&server, "matrix", 1), Some("test-token".into()))
            .unwrap();
        let key = RoomKey::new(fixture::account_id(1), "!design:example.test");
        let request = SendRequest {
            transaction: Uuid::new_v4(),
            room: key.clone(),
            body: "Exactly once".into(),
            reply_to: Some("$event:example.test".into()),
            attachments: Vec::new(),
        };
        let first = adapter.send(&request).await.unwrap();
        let second = adapter.send(&request).await.unwrap();
        assert_eq!(first, second);
        let state = server.state.lock().unwrap();
        assert_eq!(
            state
                .store
                .room(&key)
                .unwrap()
                .messages
                .iter()
                .filter(|message| message.body == "Exactly once")
                .count(),
            1
        );
        let sent = state
            .requests
            .iter()
            .find(|request| request.method == "PUT")
            .unwrap();
        assert_eq!(
            sent.body["m.relates_to"]["m.in_reply_to"]["event_id"],
            "$event:example.test"
        );
    }

    {
        let server = TestServer::start().await.unwrap();
        let adapter = Registry::default()
            .connect(config(&server, "matrix", 1), Some("test-token".into()))
            .unwrap();
        let key = RoomKey::new(fixture::account_id(1), "!design:example.test");
        server
            .state
            .lock()
            .unwrap()
            .store
            .room_mut(&key)
            .unwrap()
            .encryption = messages_core::Encryption::EndToEnd;
        let request = SendRequest {
            transaction: Uuid::new_v4(),
            room: key,
            body: "Must never become plaintext".into(),
            reply_to: None,
            attachments: Vec::new(),
        };
        assert!(matches!(
            adapter.send(&request).await,
            Err(AdapterError::Unsupported(_))
        ));
        let state = server.state.lock().unwrap();
        assert_eq!(state.requests.len(), 1);
        assert_eq!(state.requests[0].method, "GET");
        assert!(state.requests[0].path.ends_with("/state/m.room.encryption"));
        assert!(
            !state
                .store
                .conversations
                .iter()
                .flat_map(|room| &room.messages)
                .any(|message| message.body == request.body)
        );
    }
}
