use super::*;
use crate::{app::notifications::Binding, platform::notifications::Event};

#[tokio::test]
async fn workflow_inline_reply_preserves_draft_and_queues_durable_outbox() {
    let directory = tempfile::tempdir().unwrap();
    let server = TestServer::start().await.unwrap();
    let mut app = files::signal_app(directory.path(), &server).await;
    let key = app.selected.clone().unwrap();
    let room = app.store.room_mut(&key).unwrap();
    room.draft = "Unfinished desktop draft".into();
    room.reply_to = Some(room.messages[0].id.clone());
    let previous_reply = room.reply_to.clone();
    room.messages.last_mut().unwrap().id = "maya:1700000000000".into();
    let last = room.messages.last().unwrap().clone();
    let binding = Binding {
        session: app.session(key.account),
        room: key.clone(),
        through: last.timestamp.timestamp_millis(),
        message: last.id.clone(),
    };
    app.notification_bindings.insert(42, binding);
    let task = app.notification_event(Event::Reply(42, "Inline fixture reply".into()));
    assert_eq!(
        app.store.room(&key).unwrap().draft,
        "Unfinished desktop draft"
    );
    assert_eq!(app.store.room(&key).unwrap().reply_to, previous_reply);
    assert_eq!(app.store.outbox.len(), 1);
    assert_eq!(app.store.outbox[0].request.body, "Inline fixture reply");
    assert!(app.store.outbox[0].request.attachments.is_empty());
    assert_eq!(
        app.store.outbox[0].request.reply_to.as_deref(),
        Some(last.id.as_str())
    );
    drive(&mut app, task).await;
    assert!(app.store.outbox.is_empty());
    let persisted = crate::persistence::load(app.snapshot_path())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        persisted.store.room(&key).unwrap().draft,
        "Unfinished desktop draft"
    );
    let requests = server.state.lock().unwrap().requests.clone();
    assert!(requests.iter().any(|r| r.body["method"] == "send"));
}

#[tokio::test]
async fn workflow_notification_watermark_and_account_generation_reject_stale_actions() {
    let directory = tempfile::tempdir().unwrap();
    let server = TestServer::start().await.unwrap();
    let mut app = files::signal_app(directory.path(), &server).await;
    let key = app.selected.clone().unwrap();
    let room = app.store.room_mut(&key).unwrap();
    let old = room.messages.last().unwrap().clone();
    let mut newer = old.clone();
    newer.id = "newer-fixture-message".into();
    newer.own = false;
    newer.timestamp += chrono::Duration::seconds(30);
    app.store.merge_messages(&key, vec![newer.clone()]).unwrap();
    app.notification_bindings.insert(
        42,
        Binding {
            session: app.session(key.account),
            room: key.clone(),
            through: old.timestamp.timestamp_millis(),
            message: old.id.clone(),
        },
    );
    let _ = app.notification_event(Event::Action(42, "mark-read".into()));
    assert!(!app.store.message_is_read(&key, &newer));
    assert!(!app.notification_bindings.contains_key(&42));
    let stale = Binding {
        session: app.session(key.account),
        room: key.clone(),
        through: newer.timestamp.timestamp_millis(),
        message: newer.id.clone(),
    };
    app.notification_seen.insert(key.clone(), newer.id.clone());
    app.notification_since
        .insert(key.account, chrono::Utc::now());
    app.renew_session(key.account);
    assert_eq!(app.notification_seen.get(&key), Some(&newer.id));
    assert!(!app.notification_since.contains_key(&key.account));
    app.notification_bindings.insert(43, stale);
    let task = app.notification_event(Event::Reply(43, "Stale reply".into()));
    assert!(outputs(task).await.is_empty());
    assert!(app.store.outbox.is_empty());
    assert!(!app.store.message_is_read(&key, &newer));
}
