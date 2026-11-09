use messages_core::{ProtocolId, fixture};
use messages_protocols::{AdapterError, ConnectionConfig, Registry};
use messages_test_server::TestServer;

fn config(server: &TestServer, protocol: &str, index: usize) -> ConnectionConfig {
    ConnectionConfig {
        account: fixture::account_id(index),
        protocol: ProtocolId::new(protocol),
        label: protocol.into(),
        endpoint: format!("{}/{protocol}", server.url),
        user_id: "@you:example.test".into(),
        rooms: Vec::new(),
        bot: false,
        native_bridge: false,
        managed: None,
    }
}

#[tokio::test]
async fn start_matrix_creates_a_direct_room_and_resolves_an_existing_alias() {
    let server = TestServer::start().await.unwrap();
    let adapter = Registry::default()
        .connect(config(&server, "matrix", 1), Some("test-token".into()))
        .unwrap();
    assert!(
        adapter
            .start_conversation("not a Matrix identifier")
            .await
            .is_err()
    );
    assert!(server.state.lock().unwrap().requests.is_empty());
    let new = adapter
        .start_conversation("@alex:example.test")
        .await
        .unwrap();
    assert_eq!(new.key.account, fixture::account_id(1));
    assert!(new.messages.is_empty());
    assert_eq!(new.kind, messages_core::RoomKind::Direct);
    let joined = adapter
        .start_conversation("#rust:example.test")
        .await
        .unwrap();
    assert_eq!(joined.key.room, "!rust:example.test");
    let state = server.state.lock().unwrap();
    let create = &state.requests[0];
    assert_eq!(create.method, "POST");
    assert!(create.path.ends_with("/createRoom"));
    assert_eq!(create.body["invite"][0], "@alex:example.test");
    assert_eq!(create.body["is_direct"], true);
    assert!(state.store.room(&new.key).is_some());
}

#[tokio::test]
async fn start_non_matrix_lookups_and_channel_opens_are_account_scoped() {
    let server = TestServer::start().await.unwrap();
    let registry = Registry::default();
    let signal = registry
        .connect(config(&server, "signal", 0), None)
        .unwrap();
    assert!(
        signal
            .start_conversation("not a phone number")
            .await
            .is_err()
    );
    assert!(server.state.lock().unwrap().requests.is_empty());
    let conversation = signal.start_conversation("+15550100077").await.unwrap();
    assert_eq!(conversation.key.room, "+15550100077");
    assert!(matches!(
        signal.start_conversation("+15550109999").await,
        Err(AdapterError::Configuration(_))
    ));
    for (protocol, index, recipient) in [("whatsapp", 3, "+15550100077")] {
        let mut connection = config(&server, protocol, index);
        connection.account = uuid::Uuid::new_v4();
        let account = connection.account;
        let adapter = registry
            .connect(connection, Some("test-token".into()))
            .unwrap();
        let room = adapter.start_conversation(recipient).await.unwrap();
        assert_eq!(room.key.account, account);
        assert!(room.messages.is_empty());

        assert!(
            server
                .state
                .lock()
                .unwrap()
                .store
                .room(&messages_core::RoomKey::new(
                    fixture::account_id(index),
                    &room.key.room
                ))
                .is_some()
        );
    }
}
