use super::*;
use messages_protocols::SyncBatch;

#[tokio::test]
async fn delete_conversation_reclaims_attachments_and_rejects_replay_without_remote_deletion() {
    let directory = tempfile::tempdir().unwrap();
    let server = TestServer::start().await.unwrap();
    let mut app = files::signal_app(directory.path(), &server).await;
    app.window_focused = false;
    let key = app.selected.clone().unwrap();
    let old = app.store.room(&key).unwrap().clone();
    let path = directory.path().join("draft.txt");
    tokio::fs::write(&path, b"draft").await.unwrap();
    let staged =
        crate::platform::files::stage(app.file_root(&key).join("outgoing"), vec![path], 10, 1024)
            .await
            .unwrap();
    let private = staged[0].path.clone();
    app.store.room_mut(&key).unwrap().draft_attachments = staged;
    let task = app.update(Message::DeleteConversation(key.clone()));
    drive(&mut app, task).await;
    assert!(app.store.room(&key).is_none());
    assert!(!tokio::fs::try_exists(private).await.unwrap());
    let saved = crate::persistence::load(app.snapshot_path())
        .await
        .unwrap()
        .unwrap();
    assert!(saved.store.room(&key).is_none());
    app.store = saved.store;
    app.apply_sync(
        key.account,
        SyncBatch {
            conversations: vec![old.clone()],
            messages: vec![(key.clone(), old.messages.clone())],
            ..Default::default()
        },
    );
    assert!(app.store.room(&key).is_none());
    let mut future = old.messages.last().unwrap().clone();
    future.own = false;
    future.id = "new-after-deletion".into();
    future.timestamp = chrono::Utc::now() + chrono::Duration::seconds(1);
    app.apply_sync(
        key.account,
        SyncBatch {
            messages: vec![(key.clone(), vec![future])],
            ..Default::default()
        },
    );
    assert_eq!(app.store.room(&key).unwrap().messages.len(), 1);
    assert_eq!(app.store.room(&key).unwrap().unread, 1);
    assert!(
        !server
            .state
            .lock()
            .unwrap()
            .requests
            .iter()
            .any(|request| request.body["method"] == "remoteDelete")
    );
}
