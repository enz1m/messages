use super::*;
use enzui_iced_core::activation::lifecycle::StopChoice;
use std::sync::atomic::{AtomicBool, Ordering};

#[tokio::test]
async fn copy_receipt_waits_for_durable_snapshot_and_stale_or_failed_work_is_unacknowledged() {
    let directory = tempfile::tempdir().unwrap();
    let server = TestServer::start().await.unwrap();
    let mut app = super::files::signal_app(directory.path(), &server).await;
    let room = app.selected.clone().unwrap();
    let source = directory.path().join("source.txt");
    std::fs::write(&source, b"copy custody").unwrap();
    for outcome in ["saved", "failed", "stale"] {
        let ack = Arc::new(AtomicBool::new(false));
        let receipt = ack.clone();
        let task = app.stage_drop(
            app.session(room.account),
            room.clone(),
            vec![source.clone()],
            Box::new(move || receipt.store(true, Ordering::SeqCst)),
        );
        let staged = outputs(task).await;
        assert!(!ack.load(Ordering::SeqCst));
        if outcome == "stale" {
            app.renew_session(room.account);
        }
        for action in staged {
            if let Action::Output(message) = action {
                let task = app.update(message);
                let saved = outputs(task).await;
                assert!(!ack.load(Ordering::SeqCst));
                for action in saved {
                    if let Action::Output(message) = action {
                        let message = if outcome == "failed" {
                            Message::Saved(
                                app.revision,
                                Err("Synthetic durable save refusal".into()),
                            )
                        } else {
                            message
                        };
                        let task = app.update(message);
                        drive(&mut app, task).await;
                    }
                }
            }
        }
        assert_eq!(ack.load(Ordering::SeqCst), outcome == "saved");
        assert!(app.drop_admissions.is_empty());
        assert_eq!(std::fs::read(&source).unwrap(), b"copy custody");
    }
}

#[tokio::test]
async fn cancel_stop_distinguishes_predispatch_from_potentially_transmitted_sends() {
    for dispatched in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let server = TestServer::start().await.unwrap();
        let mut app = matrix_app(directory.path(), &server).await;
        paste(&mut app, "Cancel settlement");
        let send = app.update(Message::Send);
        let mut saved = outputs(send).await;
        let mut transport = Vec::new();
        if dispatched {
            for action in saved.drain(..) {
                if let Action::Output(message) = action {
                    transport.push(app.update(message));
                }
            }
        }
        app.begin_stop(StopChoice::CancelAndExit, iced::window::Id::unique());
        let stop = app.update(Message::WorkChanged);
        drive(&mut app, stop).await;
        for action in saved {
            if let Action::Output(message) = action {
                let task = app.update(message);
                drive(&mut app, task).await;
            }
        }
        drop(transport);
        if dispatched {
            assert_eq!(app.store.outbox[0].state, OutboxState::Uncertain);
        } else {
            assert!(matches!(app.store.outbox[0].state, OutboxState::Failed(_)));
        }
        assert!(server.state.lock().unwrap().requests.is_empty());
        let saved = crate::persistence::load(app.snapshot_path())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(saved.store.outbox[0].state, app.store.outbox[0].state);
    }
}

#[tokio::test]
async fn finish_stop_drains_only_queued_messages_and_never_retries_uncertain_work() {
    let directory = tempfile::tempdir().unwrap();
    let server = TestServer::start().await.unwrap();
    let mut app = matrix_app(directory.path(), &server).await;
    let room = app.selected.clone().unwrap();
    let queued = app
        .store
        .queue_message(&room, "Restored queued message".into(), chrono::Utc::now())
        .unwrap();
    let uncertain = app
        .store
        .queue_message(&room, "Never blind retry".into(), chrono::Utc::now())
        .unwrap();
    app.store.begin_send(uncertain.transaction).unwrap();
    app.store.fail_send(
        &room,
        uncertain.transaction,
        messages_core::SendError::Uncertain,
    );
    app.changed();
    let save = app.save();
    drive(&mut app, save).await;
    let window = iced::window::Id::unique();
    app.begin_stop(StopChoice::FinishThenExit, window);
    let task = app.update(Message::CloseRequested(window));
    assert_eq!(drive(&mut app, task).await, [window]);
    assert!(
        !app.store
            .outbox
            .iter()
            .any(|item| item.request.transaction == queued.transaction)
    );
    assert_eq!(
        app.store
            .outbox
            .iter()
            .find(|item| item.request.transaction == uncertain.transaction)
            .unwrap()
            .state,
        OutboxState::Uncertain
    );
    assert_eq!(
        server
            .state
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|r| r.method == "PUT" && r.path.contains("/send/"))
            .count(),
        1
    );
}
