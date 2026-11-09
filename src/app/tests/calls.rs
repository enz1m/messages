use super::*;
use crate::app::calls;
use messages_protocols::calls::CallKind;

#[tokio::test]
async fn active_call_cleanup_precedes_close_and_old_controls_cannot_touch_a_new_session() {
    use messages_protocols::calls::{CallCommand, CallPhase};
    let directory = tempfile::tempdir().unwrap();
    let server = TestServer::start().await.unwrap();
    server.state.lock().unwrap().enable_calls();
    let mut app = app_in(directory.path());
    let account = fixture::account_id(3);
    let config = ConnectionConfig {
        account,
        protocol: ProtocolId::new("whatsapp"),
        label: "Account-free call fixture".into(),
        endpoint: format!("{}/whatsapp", server.url),
        user_id: "you".into(),
        rooms: vec![],
        bot: false,
        native_bridge: false,
        managed: None,
    };
    let adapter = app
        .registry
        .connect(config.clone(), Some("test-token".into()))
        .unwrap();
    let batch = adapter.sync(None).await.unwrap();
    let room = batch
        .conversations
        .iter()
        .find(|r| r.kind == messages_core::RoomKind::Direct)
        .unwrap()
        .key
        .clone();
    app.adapters.insert(account, adapter.clone());
    app.connections.push(config);
    app.apply_sync(account, batch);
    app.selected = Some(room.clone());
    app.demo = false;
    let session = app.session(account);
    let task = app.update(Message::Call(calls::Event::Start(
        room.clone(),
        CallKind::Voice,
        session,
    )));
    drive(&mut app, task).await;
    let id = app.calls.active.as_ref().unwrap().snapshot.id;
    assert_eq!(
        app.calls.active.as_ref().unwrap().snapshot.phase,
        CallPhase::Connected
    );
    tokio::time::sleep(std::time::Duration::from_millis(110)).await;
    let poll = app.update(Message::Call(calls::Event::Tick));
    assert!(app.calls.active.as_ref().unwrap().polling);
    let mute = app.update(Message::Call(calls::Event::Mute(id, session, true)));
    drive(&mut app, mute).await;
    drive(&mut app, poll).await;
    assert!(app.calls.active.as_ref().unwrap().snapshot.muted);
    let count = server.state.lock().unwrap().requests.len();
    let _ = app.update(Message::Call(calls::Event::Mute(
        uuid::Uuid::new_v4(),
        session,
        true,
    )));
    assert_eq!(server.state.lock().unwrap().requests.len(), count);
    app.renew_session(account);
    let task = app.update(Message::WorkChanged);
    drive(&mut app, task).await;
    assert!(app.calls.active.is_none());
    assert_eq!(
        adapter
            .call_control(&room, CallCommand::Poll { id })
            .await
            .unwrap()
            .phase,
        CallPhase::Ended
    );
    let _ = app.update(Message::Call(calls::Event::Start(
        room.clone(),
        CallKind::Voice,
        session,
    )));
    assert!(app.calls.active.is_none());
    let task = app.update(Message::Call(calls::Event::Start(
        room,
        CallKind::Voice,
        app.session(account),
    )));
    drive(&mut app, task).await;
    let current = app.calls.active.as_ref().unwrap().snapshot.id;
    let window = iced::window::Id::unique();
    let task = app.update(Message::CloseRequested(window));
    let closed = drive(&mut app, task).await;
    assert_eq!(closed, vec![window]);
    assert_eq!(
        server.state.lock().unwrap().call_states[&current].1.phase,
        CallPhase::Ended
    );
}
