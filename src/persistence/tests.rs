use super::*;

#[tokio::test]
async fn persistence_rejects_symlinks_and_preserves_unsupported_or_corrupt_snapshots() {
    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("original.json");
    let link = directory.path().join("state.json");
    let valid = serde_json::to_vec(&sample()).unwrap();
    std::fs::write(&target, &valid).unwrap();
    assert!(load(target.clone()).await.unwrap().is_some());
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(load(link).await.is_err());
    assert_eq!(std::fs::read(&target).unwrap(), valid);
    std::fs::write(&target, b"private invalid snapshot").unwrap();
    assert!(load(target.clone()).await.is_err());
    assert_eq!(std::fs::read(&target).unwrap(), b"private invalid snapshot");
    let mut newer = sample();
    newer.version = 2;
    let bytes = serde_json::to_vec(&newer).unwrap();
    std::fs::write(&target, &bytes).unwrap();
    assert!(load(target.clone()).await.is_err());
    assert_eq!(std::fs::read(&target).unwrap(), bytes);
    assert!(
        load(directory.path().join("absent.json"))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn persistence_framework_write_keeps_raw_snapshot_compatibility_and_private_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.json");
    let snapshot = sample();
    save(path.clone(), snapshot).await.unwrap();
    let raw: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(raw["version"], 1);
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let restored = load(path.clone()).await.unwrap().unwrap();
    assert!(restored.demo);
    assert!(!restored.store.conversations.is_empty());
    let mut legacy = restored;
    let account = legacy.store.conversations[0].key.account;
    legacy
        .store
        .accounts
        .iter_mut()
        .find(|entry| entry.id == account)
        .unwrap()
        .protocol = messages_core::ProtocolId::new("matrix");
    let room = &mut legacy.store.conversations[0];
    let first = room.messages[0].id.clone();
    let room_id = room.key.room.clone();
    room.messages[0].content_state = None;
    room.messages[0].body = "Any legacy placeholder wording".into();
    room.messages[1].content_state = Some(messages_core::MessageContentState::Available);
    room.messages[1].body = "Waiting for encryption keys…".into();
    let typed_id = room.messages[1].id.clone();
    legacy
        .connections
        .push(messages_protocols::ConnectionConfig {
            account,
            protocol: messages_core::ProtocolId::new("matrix"),
            label: "Matrix".into(),
            endpoint: "http://127.0.0.1:9".into(),
            user_id: String::new(),
            rooms: Vec::new(),
            bot: false,
            native_bridge: true,
            managed: Some(messages_protocols::ManagedConnection::Matrix {
                homeserver: "https://example.invalid".into(),
            }),
        });
    let cache = directory.path().join("services").join(account.to_string());
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(cache.join("messages.json"),serde_json::to_vec(&serde_json::json!({"matrix":{"rooms":{room_id:{"messages":{first:{"encrypted":true},typed_id:{"encrypted":true}}}}}})).unwrap()).unwrap();
    save(path.clone(), legacy).await.unwrap();
    let restored = load(path).await.unwrap().unwrap();
    let room = &restored.store.conversations[0];
    assert!(room.messages[0].waiting_for_keys());
    assert!(room.messages[0].body.is_empty());
    assert!(!room.messages[1].waiting_for_keys());
    assert_eq!(room.messages[1].body, "Waiting for encryption keys…");
}

fn sample() -> Snapshot {
    Snapshot {
        version: 1,
        preferences: Preferences::default(),
        store: messages_core::fixture::sample_store(),
        demo: true,
        selected: None,
        dark: None,
        connections: Vec::new(),
        credential_keys: Default::default(),
        credential_cleanup: Vec::new(),
        account_purges: Vec::new(),
    }
}
