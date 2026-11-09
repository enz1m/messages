use super::*;

#[tokio::test]
async fn services_unavailable_saved_accounts_remain_read_only_without_starting_helpers_or_losing_drafts()
 {
    let directory = tempfile::tempdir().unwrap();
    let server = TestServer::start().await.unwrap();
    let mut app = matrix_app(directory.path(), &server).await;
    let account = app.store.accounts[0].id;
    app.adapters.clear();
    app.store.accounts[0].protocol = ProtocolId::new("unavailable-test");
    app.connections[0].protocol = ProtocolId::new("unavailable-test");
    let managed: messages_protocols::ManagedConnection =
        serde_json::from_value(serde_json::json!({"service":"unavailable-test"})).unwrap();
    assert!(matches!(
        managed,
        messages_protocols::ManagedConnection::Retired { .. }
    ));
    app.connections[0].managed = Some(managed);
    let encoded = serde_json::to_value(&app.connections[0]).unwrap();
    assert_eq!(encoded["managed"]["service"], "unavailable-test");
    let room = app.store.conversations[0].key.clone();
    app.store.room_mut(&room).unwrap().draft = "Keep this unsent draft".into();
    let rooms = serde_json::to_value(&app.store.conversations).unwrap();
    let task = app.restore_accounts();
    drive(&mut app, task).await;
    assert!(app.adapters.is_empty());
    assert!(app.managed_hosts.is_empty());
    assert!(!app.credential_restoring.contains(&account));
    assert!(matches!(
        app.store.accounts[0].connection,
        ConnectionState::Failed(_)
    ));
    assert!(
        !app.store
            .capabilities(&room)
            .supports(messages_core::Capability::SendText)
    );
    assert_eq!(
        serde_json::to_value(&app.store.conversations).unwrap(),
        rooms
    );
    assert_eq!(serde_json::to_value(&app.connections[0]).unwrap(), encoded);
}
