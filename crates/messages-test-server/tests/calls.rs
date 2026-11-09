use messages_core::{ProtocolId, RoomKey, fixture};
use messages_protocols::{
    ConnectionConfig, Registry,
    calls::{CallCommand, CallKind, CallPhase},
};
use messages_test_server::TestServer;
use uuid::Uuid;

#[tokio::test]
async fn call_control_is_account_room_and_request_keyed_and_ended_calls_do_not_restart() {
    let server = TestServer::start().await.unwrap();
    server.state.lock().unwrap().enable_calls();
    let account = fixture::account_id(3);
    let adapter = Registry::default()
        .connect(
            ConnectionConfig {
                account,
                protocol: ProtocolId::new("whatsapp"),
                label: "Local calls".into(),
                endpoint: format!("{}/whatsapp", server.url),
                user_id: "you".into(),
                rooms: vec![],
                bot: false,
                native_bridge: false,
                managed: None,
            },
            Some("test-token".into()),
        )
        .unwrap();
    let rooms = adapter.sync(None).await.unwrap().conversations;
    let room = rooms
        .iter()
        .find(|r| r.kind == messages_core::RoomKind::Direct)
        .unwrap()
        .key
        .clone();
    let id = Uuid::new_v4();
    let start = CallCommand::Start {
        id,
        kind: CallKind::Voice,
        devices: Default::default(),
        expires_at: chrono::Utc::now().timestamp() + 30,
    };
    let connected = adapter.call_control(&room, start.clone()).await.unwrap();
    assert_eq!(connected.phase, CallPhase::Connected);
    assert_eq!(connected.encryption, messages_core::Encryption::Sample);
    let count = server.state.lock().unwrap().requests.len();
    assert!(
        adapter
            .call_control(
                &RoomKey::new(fixture::account_id(4), &room.room),
                CallCommand::End { id }
            )
            .await
            .is_err()
    );
    assert_eq!(server.state.lock().unwrap().requests.len(), count);
    assert!(
        adapter
            .call_control(
                &RoomKey::new(account, "another-room"),
                CallCommand::End { id }
            )
            .await
            .is_err()
    );
    assert!(
        adapter
            .call_control(&room, CallCommand::Mute { id, muted: true })
            .await
            .unwrap()
            .muted
    );
    assert_eq!(
        adapter
            .call_control(&room, CallCommand::End { id })
            .await
            .unwrap()
            .phase,
        CallPhase::Ended
    );
    assert_eq!(
        adapter.call_control(&room, start).await.unwrap().phase,
        CallPhase::Ended
    );
    assert_eq!(server.state.lock().unwrap().call_states.len(), 1);
}
