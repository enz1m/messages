use super::*;
use crate::app::Dialog;

pub(super) async fn complete(app: &mut App, effects: Vec<Action<Message>>) {
    for effect in effects {
        match effect {
            Action::Output(message) => {
                let task = app.update(message);
                drive(app, task).await;
            }
            other => panic!("unexpected request effect: {other:?}"),
        }
    }
}

#[tokio::test]
async fn requests_cancelled_creation_retains_remote_room_without_stealing_a_new_draft_or_settings()
{
    let directory = tempfile::tempdir().unwrap();
    let server = TestServer::start().await.unwrap();
    let mut app = matrix_app(directory.path(), &server).await;
    let selected = app.selected.clone();
    let task = app.update(Message::Open(Dialog::NewConversation));
    drive(&mut app, task).await;
    let _ = app.update(Message::NewRecipient("@late:example.test".into()));
    let task = app.update(Message::StartConversation);
    let late = outputs(task).await;
    assert!(!late.is_empty());
    let task = app.update(Message::CloseDialog);
    drive(&mut app, task).await;
    paste(&mut app, "Keep the draft typed after cancellation");
    let task = app.update(Message::Open(Dialog::Settings));
    drive(&mut app, task).await;
    complete(&mut app, late).await;
    assert_eq!(app.selected, selected);
    assert_eq!(app.editor.text(), "Keep the draft typed after cancellation");
    assert_eq!(app.dialog, Some(Dialog::Settings));
    let saved = crate::persistence::load(app.snapshot_path())
        .await
        .unwrap()
        .unwrap();
    assert!(
        saved
            .store
            .conversations
            .iter()
            .any(|room| room.key.room.starts_with("!created-"))
    );
}
