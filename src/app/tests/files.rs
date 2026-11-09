use super::*;
use enzui_iced_widgets::file_chooser::{FileResult, FileUri, PortalOutcome};

pub(super) async fn signal_app(directory: &std::path::Path, server: &TestServer) -> App {
    let mut app = app_in(directory);
    let id = fixture::account_id(0);
    let config = ConnectionConfig {
        account: id,
        protocol: ProtocolId::new("signal"),
        label: "Signal".into(),
        endpoint: format!("{}/signal", server.url),
        user_id: "you".into(),
        rooms: Vec::new(),
        bot: false,
        native_bridge: false,
        managed: None,
    };
    let adapter = app.registry.connect(config.clone(), None).unwrap();
    app.demo = false;
    app.store.accounts.retain(|account| account.id == id);
    app.store.accounts[0].connection = ConnectionState::Online;
    app.store.accounts[0].capabilities = adapter.capabilities();
    app.store
        .conversations
        .retain(|room| room.key.account == id);
    for room in &mut app.store.conversations {
        room.capabilities = adapter.capabilities();
    }
    app.selected = Some(app.store.conversations[0].key.clone());
    app.adapters.insert(id, adapter);
    app.connections.push(config);
    app
}

pub(super) fn chooser_ticket(
    app: &mut App,
    purpose: crate::app::files::Purpose,
) -> crate::app::chooser::Ticket {
    let token = uuid::Uuid::new_v4();
    app.chooser_start.active = Some(token);
    crate::app::chooser::Ticket { token, purpose }
}

fn picked(path: &std::path::Path) -> PortalOutcome<FileResult> {
    PortalOutcome::Completed(FileResult {
        uris: vec![FileUri::new(format!("file://{}", path.display()))],
    })
}

#[tokio::test]
async fn files_selected_for_a_room_stay_with_that_room_and_survive_restart() {
    let directory = tempfile::tempdir().unwrap();
    let server = TestServer::start().await.unwrap();
    let mut app = signal_app(directory.path(), &server).await;
    let key = app.selected.clone().unwrap();
    let path = directory.path().join("report.txt");
    tokio::fs::write(&path, b"Original bytes").await.unwrap();
    let purpose = super::super::files::Purpose::Attach(app.session(key.account), key.clone());
    let ticket = chooser_ticket(&mut app, purpose);
    let task = app.update(Message::ChooserDone(ticket, picked(&path)));
    app.selected = Some(app.store.conversations[1].key.clone());
    drive(&mut app, task).await;
    assert!(app.current_room().unwrap().draft_attachments.is_empty());
    let file = &app.store.room(&key).unwrap().draft_attachments[0];
    assert_eq!(file.name, "report.txt");
    assert_eq!(
        tokio::fs::read(&file.path).await.unwrap(),
        b"Original bytes"
    );
    tokio::fs::write(&path, b"Changed original").await.unwrap();
    assert_eq!(
        tokio::fs::read(&file.path).await.unwrap(),
        b"Original bytes"
    );
    let saved = crate::persistence::load(app.snapshot_path())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        saved.store.room(&key).unwrap().draft_attachments[0].path,
        file.path
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            tokio::fs::metadata(&file.path)
                .await
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            tokio::fs::metadata(file.path.parent().unwrap())
                .await
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
}

#[tokio::test]
async fn files_send_without_text_commits_custody_and_preserves_save_refusal_across_sync() {
    let directory = tempfile::tempdir().unwrap();
    let server = TestServer::start().await.unwrap();
    let mut app = signal_app(directory.path(), &server).await;
    let key = app.selected.clone().unwrap();
    let path = directory.path().join("report.txt");
    tokio::fs::write(&path, b"Hello Signal!").await.unwrap();
    let purpose = super::super::files::Purpose::Attach(app.session(key.account), key.clone());
    let ticket = chooser_ticket(&mut app, purpose);
    let task = app.update(Message::ChooserDone(ticket, picked(&path)));
    drive(&mut app, task).await;
    let task = app.update(Message::Send);
    let saved = outputs(task).await;
    assert!(server.state.lock().unwrap().requests.is_empty());
    let snapshot = crate::persistence::load(app.snapshot_path())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.store.outbox.len(), 1);
    assert_eq!(snapshot.store.outbox[0].request.attachments.len(), 1);
    for action in saved {
        if let Action::Output(message) = action {
            let task = app.update(message);
            drive(&mut app, task).await;
        }
    }
    assert!(app.store.outbox.is_empty());
    let message = app
        .store
        .room(&key)
        .unwrap()
        .messages
        .last()
        .unwrap()
        .clone();
    assert!(message.body.is_empty());
    assert_eq!(message.attachments[0].name, "report.txt");
    let destination = directory.path().join("Saved report.txt");
    let purpose = super::super::files::Purpose::Save(
        app.session(key.account),
        key.clone(),
        message.id.clone(),
        message.attachments[0].id.clone(),
    );
    let ticket = chooser_ticket(&mut app, purpose.clone());
    let task = app.update(Message::ChooserDone(ticket, picked(&destination)));
    drive(&mut app, task).await;
    assert_eq!(
        tokio::fs::read(&destination).await.unwrap(),
        b"Hello Signal!"
    );
    let ticket = chooser_ticket(&mut app, purpose);
    let task = app.update(Message::ChooserDone(ticket, picked(&destination)));
    drive(&mut app, task).await;
    let refusal = app
        .notice
        .clone()
        .expect("existing file refusal is reported");
    let batch = app.adapters[&key.account].sync(None).await.unwrap();
    app.apply_sync(key.account, batch.clone());
    assert_eq!(app.notice.as_ref(), Some(&refusal));
    assert_eq!(
        refusal.tone(),
        enzui_iced_widgets::toast::ToastTone::Warning
    );
    assert_eq!(
        tokio::fs::read(destination).await.unwrap(),
        b"Hello Signal!"
    );
    let warning = super::super::notice::Notice::from("Connecting to your account…");
    app.notice = Some(warning.clone());
    app.apply_sync(key.account, batch.clone());
    assert_eq!(app.notice.as_ref(), Some(&warning));
    assert_eq!(
        warning.tone(),
        enzui_iced_widgets::toast::ToastTone::Warning
    );
    let mut other = app.store.accounts[0].clone();
    other.id = uuid::Uuid::new_v4();
    let other_id = other.id;
    app.store.accounts.push(other);
    let progress = super::super::notice::Notice::connecting(app.session(other_id));
    app.notice = Some(progress.clone());
    app.apply_sync(key.account, batch.clone());
    assert_eq!(app.notice.as_ref(), Some(&progress));
    let stale_session = app.session(key.account);
    app.renew_session(key.account);
    let mut progress = super::super::notice::Notice::connecting(app.session(key.account));
    progress.text = "Waiting for the connection".into();
    assert_eq!(progress.tone(), enzui_iced_widgets::toast::ToastTone::Info);
    app.notice = Some(progress.clone());
    app.clear_connection_notice(stale_session);
    assert_eq!(app.notice.as_ref(), Some(&progress));
    app.apply_sync(key.account, batch);
    assert!(app.notice.is_none());
}

#[tokio::test]
async fn files_stale_chooser_and_worker_results_cannot_attach_to_reconnected_accounts() {
    let directory = tempfile::tempdir().unwrap();
    let server = TestServer::start().await.unwrap();
    let mut app = signal_app(directory.path(), &server).await;
    let key = app.selected.clone().unwrap();
    let session = app.session(key.account);
    let path = directory.path().join("report.txt");
    tokio::fs::write(&path, b"Original bytes").await.unwrap();
    let purpose = super::super::files::Purpose::Attach(session, key.clone());
    let ticket = chooser_ticket(&mut app, purpose);
    app.renew_session(key.account);
    let task = app.update(Message::ChooserDone(ticket, picked(&path)));
    drive(&mut app, task).await;
    assert!(app.current_room().unwrap().draft_attachments.is_empty());
    let staged = crate::platform::files::stage(
        app.storage_path
            .with_file_name("files")
            .join(key.account.to_string())
            .join("outgoing"),
        vec![path],
        10,
        100,
    )
    .await
    .unwrap();
    let stored_path = staged[0].path.clone();
    let task = app.update(Message::FilesStaged(session, key, Ok(staged)));
    drive(&mut app, task).await;
    assert!(!stored_path.exists());
    assert!(app.current_room().unwrap().draft_attachments.is_empty());
}

#[tokio::test]
async fn files_publication_never_overwrites_and_private_reads_stay_in_the_account() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("original.txt");
    tokio::fs::write(&path, b"Original bytes").await.unwrap();
    assert!(
        crate::platform::files::publish(path.clone(), b"Replace me".to_vec())
            .await
            .is_err()
    );
    assert_eq!(tokio::fs::read(&path).await.unwrap(), b"Original bytes");
    let root = directory.path().join("private");
    tokio::fs::create_dir(&root).await.unwrap();
    assert!(
        crate::platform::files::read_private(root.clone(), path.to_str().unwrap())
            .await
            .is_err()
    );
    let staged = crate::platform::files::stage(root, vec![path], 10, 100)
        .await
        .unwrap();
    assert_eq!(staged[0].bytes, 14);
    assert_eq!(
        crate::platform::files::safe_name("../../photo.png"),
        "photo.png"
    );
}

#[tokio::test]
async fn files_signal_presence_respects_privacy_and_addresses_the_remote_timestamp() {
    let directory = tempfile::tempdir().unwrap();
    let server = TestServer::start().await.unwrap();
    let mut app = signal_app(directory.path(), &server).await;
    let account = app.store.accounts[0].id;
    let batch = app.adapters[&account].sync(None).await.unwrap();
    app.apply_sync(account, batch);
    server.state.lock().unwrap().requests.clear();
    app.preferences.read_receipts = false;
    app.preferences.typing_indicators = false;
    let task = app.presence_tick();
    drive(&mut app, task).await;
    let task = app.composer_typing();
    drive(&mut app, task).await;
    assert!(server.state.lock().unwrap().requests.is_empty());
    app.preferences.read_receipts = true;
    app.window_focused = false;
    let task = app.presence_tick();
    drive(&mut app, task).await;
    assert!(server.state.lock().unwrap().requests.is_empty());
    app.window_focused = true;
    server.state.lock().unwrap().next_failure = Some(503);
    let task = app.presence_tick();
    drive(&mut app, task).await;
    assert!(app.read_sent.is_empty());
    let task = app.presence_tick();
    drive(&mut app, task).await;
    {
        let state = server.state.lock().unwrap();
        assert_eq!(state.requests.len(), 2);
        assert_eq!(state.requests[1].body["method"], "sendReceipt");
        assert_eq!(state.requests[1].body["params"]["type"], "read");
        assert!(
            state.requests[1].body["params"]["targetTimestamp"][0]
                .as_i64()
                .unwrap()
                > 0
        );
    }
    let task = app.presence_tick();
    drive(&mut app, task).await;
    assert_eq!(server.state.lock().unwrap().requests.len(), 2);
    app.preferences.typing_indicators = true;
    app.editor = text_editor::Content::with_text("Typing a reply");
    let task = app.composer_typing();
    drive(&mut app, task).await;
    assert_eq!(
        server.state.lock().unwrap().requests.last().unwrap().body["params"]["stop"],
        false
    );
    app.editor = text_editor::Content::new();
    let task = app.composer_typing();
    drive(&mut app, task).await;
    assert_eq!(
        server.state.lock().unwrap().requests.last().unwrap().body["params"]["stop"],
        true
    );
}

#[tokio::test]
async fn files_removing_a_draft_reclaims_its_copy_only_after_the_snapshot_commits() {
    let directory = tempfile::tempdir().unwrap();
    let server = TestServer::start().await.unwrap();
    let mut app = signal_app(directory.path(), &server).await;
    let key = app.selected.clone().unwrap();
    let original = directory.path().join("Original.txt");
    tokio::fs::write(&original, b"Original bytes")
        .await
        .unwrap();
    let purpose = super::super::files::Purpose::Attach(app.session(key.account), key.clone());
    let ticket = chooser_ticket(&mut app, purpose);
    let task = app.update(Message::ChooserDone(ticket, picked(&original)));
    drive(&mut app, task).await;
    let file = app.current_room().unwrap().draft_attachments[0].clone();
    let task = app.update(Message::RemoveAttachment(file.id));
    let completed = outputs(task).await;
    assert!(file.path.exists());
    for action in completed {
        if let Action::Output(message) = action {
            let task = app.update(message);
            drive(&mut app, task).await;
        }
    }
    assert!(!file.path.exists());
    assert_eq!(tokio::fs::read(original).await.unwrap(), b"Original bytes");
    assert!(
        crate::persistence::load(app.snapshot_path())
            .await
            .unwrap()
            .unwrap()
            .store
            .room(&key)
            .unwrap()
            .draft_attachments
            .is_empty()
    );
}
