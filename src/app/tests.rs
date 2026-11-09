use std::{collections::VecDeque, sync::Arc, time::Duration};

use iced::{Task, futures::StreamExt, widget::text_editor};
use iced_runtime::Action;
use messages_core::{ConnectionState, Delivery, OutboxState, ProtocolId, fixture};
use messages_protocols::ConnectionConfig;
use messages_test_server::TestServer;

use super::{App, Message};
mod background;
mod calls;
mod conversations;
mod credentials;
mod erasure;
mod files;
mod history;
mod login;
mod native_admission;
mod requests;
mod services;
mod shutdown;
mod virtualization;
mod workflows;

fn app_in(directory: &std::path::Path) -> App {
    let (mut app, _) = App::new();
    app.loading = false;
    app.account_form.managed = false;
    app.storage_path = directory.join("state.json");
    app.theme_writer.path = Some(directory.join("config/enzui/io.enz.Messages.toml"));
    app
}

async fn outputs(task: Task<Message>) -> Vec<Action<Message>> {
    tokio::time::timeout(Duration::from_secs(15), async {
        if let Some(mut stream) = iced_runtime::task::into_stream(task) {
            let mut effects = Vec::new();
            while let Some(effect) = stream.next().await {
                // Dropping widget operations releases completion channels without a live tree.
                if !matches!(effect, Action::Widget(_)) {
                    effects.push(effect);
                }
            }
            effects
        } else {
            Vec::new()
        }
    })
    .await
    .expect("application task exceeded test deadline")
}

async fn drive(app: &mut App, task: Task<Message>) -> Vec<iced::window::Id> {
    let mut work = VecDeque::from([task]);
    let mut closed = Vec::new();
    let mut count = 0;
    while let Some(task) = work.pop_front() {
        count += 1;
        assert!(count < 100, "application reducer failed to settle");
        for action in outputs(task).await {
            match action {
                Action::Output(message) => work.push_back(app.update(message)),
                Action::Window(iced_runtime::window::Action::Close(id)) => closed.push(id),
                Action::Widget(_) => {}
                other => panic!("unexpected app effect: {other:?}"),
            }
        }
    }
    closed
}

fn paste(app: &mut App, text: &str) {
    let _ = app.update(Message::Editor(text_editor::Action::Edit(
        text_editor::Edit::Paste(Arc::new(text.into())),
    )));
}

async fn matrix_app(directory: &std::path::Path, server: &TestServer) -> App {
    let mut app = app_in(directory);
    let id = fixture::account_id(1);
    let config = ConnectionConfig {
        account: id,
        protocol: ProtocolId::new("matrix"),
        label: "Matrix".into(),
        endpoint: format!("{}/matrix", server.url),
        user_id: "@you:example.test".into(),
        rooms: Vec::new(),
        bot: false,
        native_bridge: false,
        managed: None,
    };
    app.demo = false;
    app.store.accounts.retain(|account| account.id == id);
    app.store.accounts[0].connection = ConnectionState::Online;
    app.store
        .conversations
        .retain(|room| room.key.account == id);
    app.selected = Some(app.store.conversations[0].key.clone());
    app.adapters.insert(
        id,
        app.registry
            .connect(config.clone(), Some("test-token".into()))
            .unwrap(),
    );
    app.connections.push(config);
    app
}

#[tokio::test]
async fn lifecycle_send_is_durable_before_http_and_recovers_without_blind_retry() {
    let directory = tempfile::tempdir().unwrap();
    let server = TestServer::start().await.unwrap();
    let mut app = matrix_app(directory.path(), &server).await;
    paste(&mut app, "Persist before transport");
    let task = app.update(Message::Send);
    let mut saved = outputs(task).await;
    assert_eq!(saved.len(), 1);
    assert!(server.state.lock().unwrap().requests.is_empty());
    let raw: crate::persistence::Snapshot =
        serde_json::from_slice(&std::fs::read(app.snapshot_path()).unwrap()).unwrap();
    assert_eq!(raw.store.outbox.len(), 1);
    assert_eq!(raw.store.outbox[0].state, OutboxState::Sending);
    let recovered = crate::persistence::load(app.snapshot_path())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered.store.outbox[0].state, OutboxState::Uncertain);
    let Action::Output(message) = saved.pop().unwrap() else {
        panic!("expected save completion")
    };
    let task = app.update(message);
    drive(&mut app, task).await;
    {
        let state = server.state.lock().unwrap();
        assert_eq!(
            state
                .requests
                .iter()
                .filter(|request| request.method == "PUT" && request.path.contains("/send/"))
                .count(),
            1
        );
        assert!(
            state
                .store
                .conversations
                .iter()
                .flat_map(|room| &room.messages)
                .any(|message| message.body == "Persist before transport")
        );
    }
    let finished = crate::persistence::load(app.snapshot_path())
        .await
        .unwrap()
        .unwrap();
    assert!(finished.store.outbox.is_empty());
    assert!(
        finished
            .store
            .conversations
            .iter()
            .flat_map(|room| &room.messages)
            .any(|message| message.body == "Persist before transport"
                && message.delivery == Delivery::Sent)
    );
}

#[tokio::test]
async fn lifecycle_storage_failure_prevents_transport_and_keeps_the_window_open() {
    let directory = tempfile::tempdir().unwrap();
    let server = TestServer::start().await.unwrap();
    let mut app = matrix_app(directory.path(), &server).await;
    let blocked = directory.path().join("blocked");
    std::fs::write(&blocked, "a file where the directory should be").unwrap();
    app.storage_path = blocked.join("state.json");
    paste(&mut app, "Keep this text if storage fails");
    let task = app.update(Message::Send);
    assert!(drive(&mut app, task).await.is_empty());
    let task = app.update(Message::CloseRequested(iced::window::Id::unique()));
    assert!(drive(&mut app, task).await.is_empty());
    assert!(server.state.lock().unwrap().requests.is_empty());
    assert!(
        app.store
            .outbox
            .iter()
            .any(|item| item.request.body == "Keep this text if storage fails")
    );
    assert!(app.notice.is_some());
}

#[tokio::test]
async fn lifecycle_failed_edit_retains_text_and_successful_retry_restores_draft() {
    let directory = tempfile::tempdir().unwrap();
    let server = TestServer::start().await.unwrap();
    let mut app = matrix_app(directory.path(), &server).await;
    let key = app.selected.clone().unwrap();
    let id = app
        .store
        .room(&key)
        .unwrap()
        .messages
        .iter()
        .find(|message| message.own)
        .unwrap()
        .id
        .clone();
    app.store.room_mut(&key).unwrap().draft = "Unrelated composition draft".into();
    let task = app.update(Message::Edit(id.clone()));
    drive(&mut app, task).await;
    app.editor = text_editor::Content::with_text("Corrected message text");
    server.state.lock().unwrap().next_failure = Some(500);
    let task = app.update(Message::Send);
    drive(&mut app, task).await;
    assert_eq!(app.editor.text(), "Corrected message text");
    assert_eq!(app.editing.as_ref().unwrap().id, id);
    assert_ne!(
        app.store
            .room(&key)
            .unwrap()
            .messages
            .iter()
            .find(|message| message.id == id)
            .unwrap()
            .body,
        "Corrected message text"
    );
    let task = app.update(Message::Send);
    drive(&mut app, task).await;
    assert!(app.editing.is_none());
    assert_eq!(app.editor.text(), "Unrelated composition draft");
    assert_eq!(
        app.store
            .room(&key)
            .unwrap()
            .messages
            .iter()
            .find(|message| message.id == id)
            .unwrap()
            .body,
        "Corrected message text"
    );
}

#[tokio::test]
async fn lifecycle_reconnect_rejects_old_sync_without_clearing_the_new_inflight_flag() {
    let directory = tempfile::tempdir().unwrap();
    let server = TestServer::start().await.unwrap();
    let mut app = matrix_app(directory.path(), &server).await;
    let account = fixture::account_id(1);
    let old_task = app.sync_account(account);
    let mut old = outputs(old_task).await;
    app.renew_session(account);
    app.store.conversations[0].title = "New session's local state".into();
    let current_task = app.sync_account(account);
    let Action::Output(old) = old.pop().unwrap() else {
        panic!("missing sync completion")
    };
    let ignored = app.update(old);
    drive(&mut app, ignored).await;
    assert!(app.syncing.contains(&account));
    assert_eq!(
        app.store.conversations[0].title,
        "New session's local state"
    );
    drive(&mut app, current_task).await;
    assert!(!app.syncing.contains(&account));
    assert_ne!(
        app.store.conversations[0].title,
        "New session's local state"
    );
}

#[tokio::test]
async fn lifecycle_account_identity_change_does_not_mix_remote_conversations() {
    let directory = tempfile::tempdir().unwrap();
    let server = TestServer::start().await.unwrap();
    let mut app = matrix_app(directory.path(), &server).await;
    let account = app.store.accounts[0].id;
    let original = app.store.conversations[0].clone();
    let mut incoming = original.clone();
    incoming.title = "Another person's room".into();
    incoming.messages.clear();
    let task = app.update(Message::Synced(
        account,
        Ok(messages_protocols::SyncBatch {
            user_id: Some("@someone_else:example.test".into()),
            conversations: vec![incoming],
            ..Default::default()
        }),
    ));
    drive(&mut app, task).await;
    assert_eq!(app.store.conversations[0].title, original.title);
    assert_eq!(
        app.store.conversations[0].messages.len(),
        original.messages.len()
    );
    assert_eq!(app.store.accounts[0].user_id, "@you:example.test");
    assert!(!app.adapters.contains_key(&account));
    assert!(matches!(
        app.store.accounts[0].connection,
        ConnectionState::Failed(_)
    ));
}
