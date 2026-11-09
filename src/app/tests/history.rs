use super::*;

#[tokio::test]
async fn history_pages_merge_persist_and_ignore_sync_cursor_rewinds() {
    let directory = tempfile::tempdir().unwrap();
    let server = TestServer::start().await.unwrap();
    server.state.lock().unwrap().seed_history(123);
    let mut app = matrix_app(directory.path(), &server).await;
    app.store.conversations.clear();
    let account = fixture::account_id(1);
    let task = app.sync_account(account);
    drive(&mut app, task).await;
    let key = app.store.conversations[0].key.clone();
    app.selected = Some(key.clone());
    assert_eq!(app.store.room(&key).unwrap().messages.len(), 20);
    assert_eq!(
        app.store.room(&key).unwrap().pagination.as_deref(),
        Some("offset:103")
    );
    let task = app.update(Message::LoadOlder(key.clone()));
    let duplicate = app.update(Message::LoadOlder(key.clone()));
    assert!(outputs(duplicate).await.is_empty());
    drive(&mut app, task).await;
    assert_eq!(app.store.room(&key).unwrap().messages.len(), 70);
    assert_eq!(
        app.store.room(&key).unwrap().pagination.as_deref(),
        Some("offset:53")
    );
    let task = app.sync_account(account);
    drive(&mut app, task).await;
    assert_eq!(
        app.store.room(&key).unwrap().pagination.as_deref(),
        Some("offset:53")
    );
    for _ in 0..2 {
        let task = app.update(Message::LoadOlder(key.clone()));
        drive(&mut app, task).await;
    }
    assert_eq!(app.store.room(&key).unwrap().messages.len(), 123);
    assert!(!app.store.room(&key).unwrap().has_older);
    let task = app.sync_account(account);
    drive(&mut app, task).await;
    assert!(!app.store.room(&key).unwrap().has_older);
    let saved = crate::persistence::load(app.snapshot_path())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.store.room(&key).unwrap().messages.len(), 123);
    assert!(!saved.store.room(&key).unwrap().has_older);
}
