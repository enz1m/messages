use super::*;

#[tokio::test]
async fn shutdown_waits_for_accepted_attachment_staging_and_its_snapshot() {
    use enzui_iced_widgets::file_chooser::{FileResult, FileUri, PortalOutcome};
    let directory = tempfile::tempdir().unwrap();
    let server = TestServer::start().await.unwrap();
    let mut app = super::files::signal_app(directory.path(), &server).await;
    let key = app.selected.clone().unwrap();
    let source = directory.path().join("custody.txt");
    std::fs::write(&source, b"synthetic custody bytes").unwrap();
    let purpose = crate::app::files::Purpose::Attach(app.session(key.account), key.clone());
    let ticket = super::files::chooser_ticket(&mut app, purpose);
    let staged = app.update(Message::ChooserDone(
        ticket,
        PortalOutcome::Completed(FileResult {
            uris: vec![FileUri::new(format!("file://{}", source.display()))],
        }),
    ));
    assert!(app.file_busy.contains(&key));
    let window = iced::window::Id::unique();
    let close = app.update(Message::CloseRequested(window));
    assert!(drive(&mut app, close).await.is_empty());
    assert_eq!(drive(&mut app, staged).await, [window]);
    let saved = crate::persistence::load(app.snapshot_path())
        .await
        .unwrap()
        .unwrap();
    let files = &saved.store.room(&key).unwrap().draft_attachments;
    assert_eq!(files.len(), 1);
    assert_eq!(
        std::fs::read(&files[0].path).unwrap(),
        b"synthetic custody bytes"
    );
    assert_eq!(std::fs::read(source).unwrap(), b"synthetic custody bytes");
}
