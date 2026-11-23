use super::*;
use crate::app::Dialog;

#[tokio::test]
async fn sample_banner_visibility_and_connect_action_preserve_snapshot_separation() {
    let directory = tempfile::tempdir().unwrap();
    let server = TestServer::start().await.unwrap();
    let mut app = app_in(directory.path());
    let sample_room = app.current_room().expect("sample room selected").clone();

    assert!(app.demo);
    assert!(crate::view::has_conversation_banner(&app, &sample_room));
    assert!(!crate::view::show_sidebar_sample_banner(&app, false));
    assert!(crate::view::show_sidebar_sample_banner(&app, true));
    assert_eq!(
        app.snapshot_path(),
        directory.path().join("sample-state.json")
    );

    let _ = app.update(Message::Open(Dialog::ConnectAccount));
    assert_eq!(app.dialog, Some(Dialog::ConnectAccount));
    assert!(server.state.lock().unwrap().requests.is_empty());

    let _ = app.update(Message::CloseDialog);
    assert_eq!(app.dialog, None);
    assert!(app.demo);

    let _ = app.update(Message::Open(Dialog::ConnectAccount));
    app.account_form.protocol = "matrix".into();
    app.account_form.managed = false;
    app.account_form.native_bridge = false;
    app.account_form.endpoint = format!("{}/matrix", server.url);
    app.account_form.user = "@you:example.test".into();
    app.account_form.token = "test-token".into();
    let task = app.update(Message::Connect);
    drive(&mut app, task).await;

    assert!(!app.demo);
    assert_eq!(app.snapshot_path(), directory.path().join("state.json"));
    assert!(directory.path().join("sample-state.json").exists());
    let real_room = app.current_room().expect("real room selected");
    assert!(!crate::view::has_conversation_banner(&app, real_room));
    assert!(!crate::view::show_sidebar_sample_banner(&app, false));
    assert!(!crate::view::show_sidebar_sample_banner(&app, true));
}
