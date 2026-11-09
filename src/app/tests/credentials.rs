use enzui_services::credential_store::{MemoryCredentialStore, UnavailableCredentialStore};

use super::*;
use crate::platform::credentials::Credentials;

fn form(app: &mut App, server: &TestServer) {
    app.account_form.protocol = "matrix".into();
    app.account_form.native_bridge = false;
    app.account_form.endpoint = format!("{}/matrix", server.url);
    app.account_form.user = "@you:example.test".into();
    app.account_form.token = "test-token".into();
    app.account_form.remember_token = true;
}

#[tokio::test]
async fn credentials_connect_restore_and_remove_keep_tokens_out_of_snapshots() {
    let directory = tempfile::tempdir().unwrap();
    let server = TestServer::start().await.unwrap();
    let vault = Arc::new(MemoryCredentialStore::new());
    let mut app = app_in(directory.path());
    app.credentials = Some(Credentials::with_store(vault.clone()));
    let task = app.update(Message::Open(crate::app::Dialog::ConnectAccount));
    drive(&mut app, task).await;
    form(&mut app, &server);
    let task = app.update(Message::Connect);
    drive(&mut app, task).await;
    assert!(app.dialog.is_none());
    assert_eq!(vault.entry_count(), 1);
    assert_eq!(app.credential_keys.len(), 1);
    assert!(app.credential_cleanup.is_empty());
    let account = app.store.accounts[0].id;
    let snapshot = crate::persistence::load(app.snapshot_path()).await.unwrap();
    assert!(
        !std::fs::read_to_string(app.snapshot_path())
            .unwrap()
            .contains("test-token")
    );
    let mut restored = app_in(directory.path());
    restored.credentials = Some(Credentials::with_store(vault.clone()));
    let task = restored.update(Message::Loaded(Ok(snapshot)));
    drive(&mut restored, task).await;
    assert!(restored.adapters.contains_key(&account));
    assert_eq!(
        restored.store.accounts[0].connection,
        ConnectionState::Online
    );
    assert_eq!(vault.get_call_count(), 1);
    let task = restored.update(Message::RemoveAccount(account));
    let effects = outputs(task).await;
    assert_eq!(
        vault.entry_count(),
        1,
        "cleanup must await the durable account removal"
    );
    for effect in effects {
        let Action::Output(message) = effect else {
            panic!("unexpected effect")
        };
        let task = restored.update(message);
        drive(&mut restored, task).await;
    }
    assert_eq!(vault.entry_count(), 0);
    assert!(restored.credential_cleanup.is_empty());
    let snapshot = crate::persistence::load(restored.snapshot_path())
        .await
        .unwrap()
        .unwrap();
    assert!(snapshot.store.accounts.is_empty());
    assert!(snapshot.credential_keys.is_empty());
    assert!(snapshot.credential_cleanup.is_empty());
}

#[tokio::test]
async fn credentials_unavailable_keyring_keeps_a_working_session_without_plaintext_fallback() {
    let directory = tempfile::tempdir().unwrap();
    let server = TestServer::start().await.unwrap();
    let mut app = app_in(directory.path());
    app.credentials = Some(Credentials::with_store(Arc::new(
        UnavailableCredentialStore,
    )));
    form(&mut app, &server);
    let task = app.update(Message::Connect);
    drive(&mut app, task).await;
    assert_eq!(app.adapters.len(), 1);
    assert!(app.credential_keys.is_empty());
    assert!(
        app.credential_notice
            .as_deref()
            .unwrap()
            .contains("this session only")
    );
    assert!(
        !std::fs::read_to_string(app.snapshot_path())
            .unwrap()
            .contains("test-token")
    );
    assert_eq!(app.credential_failed.len(), 1);
    let task = app.update(Message::Tick);
    drive(&mut app, task).await;
    assert!(app.credential_busy.is_empty());
    assert_eq!(app.credential_failed.len(), 1);
}

#[tokio::test]
async fn credentials_restart_retires_a_write_interrupted_before_binding_commit() {
    let directory = tempfile::tempdir().unwrap();
    let server = TestServer::start().await.unwrap();
    let vault = Arc::new(MemoryCredentialStore::new());
    let mut app = matrix_app(directory.path(), &server).await;
    app.credentials = Some(Credentials::with_store(vault.clone()));
    let account = app.store.accounts[0].id;
    app.stage_credential(account, Some("test-token".into()), true);
    app.changed();
    let effects = outputs(app.save()).await;
    assert_eq!(vault.entry_count(), 0);
    let Action::Output(saved) = effects.into_iter().next().unwrap() else {
        panic!("expected snapshot completion")
    };
    let task = app.update(saved);
    let effects = outputs(task).await;
    assert!(effects.iter().any(|effect| matches!(
        effect,
        Action::Output(Message::WorkCompleted(_, message)) if matches!(message.as_ref(), Message::CredentialStored(_, _, Ok(())))
    )));
    assert_eq!(vault.entry_count(), 1);
    drop(effects);
    let snapshot = crate::persistence::load(app.snapshot_path()).await.unwrap();
    assert!(snapshot.as_ref().unwrap().credential_keys.is_empty());
    assert_eq!(snapshot.as_ref().unwrap().credential_cleanup.len(), 1);
    let mut restarted = app_in(directory.path());
    restarted.credentials = Some(Credentials::with_store(vault.clone()));
    let task = restarted.update(Message::Loaded(Ok(snapshot)));
    drive(&mut restarted, task).await;
    assert_eq!(vault.entry_count(), 0);
    assert!(restarted.credential_cleanup.is_empty());
}

#[tokio::test]
async fn credentials_snapshot_failure_blocks_keyring_mutation() {
    let directory = tempfile::tempdir().unwrap();
    let server = TestServer::start().await.unwrap();
    let vault = Arc::new(MemoryCredentialStore::new());
    let mut app = matrix_app(directory.path(), &server).await;
    app.credentials = Some(Credentials::with_store(vault.clone()));
    let blocker = directory.path().join("blocker");
    std::fs::write(&blocker, "file, not directory").unwrap();
    app.storage_path = blocker.join("state.json");
    app.stage_credential(app.store.accounts[0].id, Some("test-token".into()), true);
    app.changed();
    let task = app.save();
    drive(&mut app, task).await;
    assert_eq!(vault.entry_count(), 0);
    assert!(app.credential_keys.is_empty());
    assert_eq!(app.credential_pending.len(), 1);
    assert!(app.notice.is_some());
}

async fn pending_write(
    directory: &std::path::Path,
    server: &TestServer,
) -> (
    App,
    Arc<MemoryCredentialStore>,
    Arc<enzui_services::credential_store::Gate>,
    Task<Message>,
) {
    let gate = enzui_services::credential_store::Gate::new_closed();
    let vault = Arc::new(MemoryCredentialStore::with_gate(gate.clone()));
    let mut app = matrix_app(directory, server).await;
    app.credentials = Some(Credentials::with_store(vault.clone()));
    app.stage_credential(app.store.accounts[0].id, Some("test-token".into()), true);
    app.changed();
    let mut saved = outputs(app.save()).await;
    let Action::Output(saved) = saved.pop().unwrap() else {
        panic!("expected snapshot completion")
    };
    let task = app.update(saved);
    (app, vault, gate, task)
}

#[tokio::test]
async fn credentials_removal_overlaps_a_pending_write_and_retires_its_late_success() {
    use iced::futures::FutureExt;
    let directory = tempfile::tempdir().unwrap();
    let server = TestServer::start().await.unwrap();
    let (mut app, vault, gate, task) = pending_write(directory.path(), &server).await;
    let mut writer = Box::pin(outputs(task));
    assert!(writer.as_mut().now_or_never().is_none());
    let task = app.update(Message::RemoveAccount(app.store.accounts[0].id));
    drive(&mut app, task).await;
    assert_eq!(app.credential_busy.len(), 1);
    assert!(app.credential_keys.is_empty());
    gate.open();
    let effects = writer.await;
    assert_eq!(vault.entry_count(), 1);
    for effect in effects {
        let Action::Output(message) = effect else {
            panic!("unexpected effect")
        };
        let task = app.update(message);
        drive(&mut app, task).await;
    }
    assert_eq!(vault.entry_count(), 0);
    assert!(app.credential_keys.is_empty());
    assert!(app.credential_cleanup.is_empty());
    assert!(app.store.accounts.is_empty());
}

#[tokio::test]
async fn credentials_close_waits_for_the_binding_snapshot_after_a_pending_write() {
    use iced::futures::FutureExt;
    let directory = tempfile::tempdir().unwrap();
    let server = TestServer::start().await.unwrap();
    let (mut app, vault, gate, task) = pending_write(directory.path(), &server).await;
    let mut writer = Box::pin(outputs(task));
    assert!(writer.as_mut().now_or_never().is_none());
    let id = iced::window::Id::unique();
    let task = app.update(Message::CloseRequested(id));
    assert!(drive(&mut app, task).await.is_empty());
    gate.open();
    let mut closed = Vec::new();
    for effect in writer.await {
        let Action::Output(message) = effect else {
            panic!("unexpected effect")
        };
        let task = app.update(message);
        closed.extend(drive(&mut app, task).await);
    }
    assert_eq!(closed, [id]);
    assert_eq!(vault.entry_count(), 1);
    let saved = crate::persistence::load(app.snapshot_path())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.credential_keys.len(), 1);
    assert!(saved.credential_cleanup.is_empty());
}
