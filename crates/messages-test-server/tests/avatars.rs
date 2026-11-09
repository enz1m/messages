use messages_core::{ProtocolId, RoomKey, fixture};
use messages_protocols::{ConnectionConfig, Registry};
use messages_test_server::TestServer;
#[tokio::test]
async fn bridge_photos_are_room_keyed_bounded_optional_and_reject_another_account_before_dispatch()
{
    let server = TestServer::start().await.unwrap();
    let account = fixture::account_id(3);
    let key = RoomKey::new(account, "photo-peer");
    let config = ConnectionConfig {
        account,
        protocol: ProtocolId::new("whatsapp"),
        label: "Synthetic".into(),
        endpoint: format!("{}/whatsapp", server.url),
        user_id: "you".into(),
        rooms: vec![],
        bot: false,
        native_bridge: true,
        managed: None,
    };
    let adapter = Registry::default()
        .connect(config, Some("test-token".into()))
        .unwrap();
    assert!(adapter.avatar(&key).await.unwrap().is_none());
    server
        .state
        .lock()
        .unwrap()
        .avatar_data
        .insert(key.room.clone(), vec![1, 2, 3]);
    assert_eq!(adapter.avatar(&key).await.unwrap(), Some(vec![1, 2, 3]));
    let before = server.state.lock().unwrap().requests.len();
    assert!(
        adapter
            .avatar(&RoomKey::new(fixture::account_id(0), &key.room))
            .await
            .is_err()
    );
    assert_eq!(server.state.lock().unwrap().requests.len(), before);
    server
        .state
        .lock()
        .unwrap()
        .avatar_data
        .insert(key.room.clone(), vec![0; 262145]);
    assert!(adapter.avatar(&key).await.is_err());
}
