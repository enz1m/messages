//! agent_c/enz-messages#2: removing an account erases its helper storage and
//! files only behind a durable tombstone and a stop/settlement barrier.

use super::*;

/// Seed the account-owned directories with representative contents.
fn seed(directory: &std::path::Path, account: uuid::Uuid) -> [std::path::PathBuf; 2] {
    let [services, files] = crate::platform::services::account_directories(directory, account);
    std::fs::create_dir_all(services.join("signal/data")).unwrap();
    std::fs::write(services.join("whatsapp.db"), b"sdk sqlite").unwrap();
    std::fs::write(services.join("signal/data/keys"), b"keys").unwrap();
    std::fs::create_dir_all(files.join("outgoing/batch/item")).unwrap();
    std::fs::write(files.join("outgoing/batch/item/photo.jpg"), b"staged").unwrap();
    std::fs::create_dir_all(files.join("opened/item")).unwrap();
    std::fs::write(files.join("opened/item/report.pdf"), b"opened").unwrap();
    [services, files]
}

/// A helper that keeps writing into its storage until it is interrupted.
fn writing_helper(services: &std::path::Path) -> Arc<crate::platform::services::Host> {
    let child = tokio::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(r#"trap 'exit 0' INT; while :; do echo tick >> "$1/journal"; sleep 0.02; done"#)
        .arg("helper")
        .arg(services)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    Arc::new(crate::platform::services::Host::from_child(child))
}

async fn saved(app: &App) -> crate::persistence::Snapshot {
    crate::persistence::load(app.snapshot_path())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn erasure_removal_stops_the_helper_then_erases_only_that_accounts_storage() {
    let directory = tempfile::tempdir().unwrap();
    let server = TestServer::start().await.unwrap();
    let mut app = matrix_app(directory.path(), &server).await;
    let account = app.store.accounts[0].id;
    let other = uuid::Uuid::new_v4();
    let [services, files] = seed(directory.path(), account);
    let [other_services, other_files] = seed(directory.path(), other);
    let host = writing_helper(&services);
    app.managed_hosts.insert(account, host.clone());

    let task = app.update(Message::RemoveAccount(account));
    drive(&mut app, task).await;

    assert!(host.is_stopped(), "the helper is stopped before erasure");
    assert!(
        !services.exists(),
        "SDK storage, keys and config are erased"
    );
    assert!(!files.exists(), "staged and opened attachments are erased");
    assert!(other_services.join("whatsapp.db").exists());
    assert!(other_files.join("opened/item/report.pdf").exists());
    assert!(app.erase_hosts.is_empty() && app.erasing.is_empty());
    // The settings row is gone, but the tombstone stays until the next start.
    let snapshot = saved(&app).await;
    assert!(snapshot.store.accounts.is_empty());
    assert_eq!(snapshot.account_purges, [account]);

    // The next start sweeps again (a late writer may have recreated a name)
    // and only then retires the tombstone.
    std::fs::create_dir_all(files.join("opened/late")).unwrap();
    let mut restored = app_in(directory.path());
    let task = restored.update(Message::Loaded(Ok(Some(snapshot))));
    drive(&mut restored, task).await;
    assert!(!files.exists(), "the start-up sweep erases a late leftover");
    assert!(restored.account_purges.is_empty());
    assert!(saved(&restored).await.account_purges.is_empty());
    assert!(other_services.join("whatsapp.db").exists());
}

#[tokio::test]
async fn erasure_waits_for_the_committed_removal_and_live_setup_or_file_work() {
    let directory = tempfile::tempdir().unwrap();
    let server = TestServer::start().await.unwrap();
    let mut app = matrix_app(directory.path(), &server).await;
    let account = app.store.accounts[0].id;
    let [services, files] = seed(directory.path(), account);

    // A stale account setup and an attachment write are still settling.
    let setup = app.jobs.begin(crate::app::jobs::Kind::AccountSetup);
    let file = app.jobs.begin(crate::app::jobs::Kind::File);

    // Before the removal snapshot commits, nothing is erased.
    let task = app.update(Message::RemoveAccount(account));
    assert!(services.exists() && files.exists());
    assert_eq!(app.account_purges, [account]);
    drive(&mut app, task).await;
    assert_eq!(saved(&app).await.account_purges, [account]);
    assert!(
        services.exists() && files.exists(),
        "live setup or file work holds the barrier"
    );

    let task = app.update(Message::WorkCompleted(
        setup,
        Box::new(Message::WorkChanged),
    ));
    drive(&mut app, task).await;
    assert!(services.exists(), "the file job still holds the barrier");

    let task = app.update(Message::WorkCompleted(file, Box::new(Message::WorkChanged)));
    drive(&mut app, task).await;
    assert!(!services.exists() && !files.exists());
}

#[tokio::test]
async fn erasure_sweep_never_deletes_a_saved_account_or_follows_links() {
    let directory = tempfile::tempdir().unwrap();
    let server = TestServer::start().await.unwrap();
    let app = matrix_app(directory.path(), &server).await;
    let live = app.store.accounts[0].id;
    let removed = uuid::Uuid::new_v4();
    let [live_services, live_files] = seed(directory.path(), live);
    // The removed account's storage name is a link to data outside it.
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("precious"), b"keep").unwrap();
    let [removed_services, removed_files] =
        crate::platform::services::account_directories(directory.path(), removed);
    std::fs::create_dir_all(removed_services.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(outside.path(), &removed_services).unwrap();
    std::fs::create_dir_all(removed_files.join("outgoing")).unwrap();

    let mut snapshot = crate::persistence::Snapshot {
        version: 1,
        preferences: Default::default(),
        store: app.store.clone(),
        demo: false,
        selected: None,
        dark: None,
        connections: Vec::new(),
        credential_keys: Default::default(),
        credential_cleanup: Vec::new(),
        account_purges: vec![live, removed],
    };
    snapshot.store.outbox.clear();
    let mut restored = app_in(directory.path());
    let task = restored.update(Message::Loaded(Ok(Some(snapshot))));
    drive(&mut restored, task).await;

    assert!(
        live_services.join("whatsapp.db").exists(),
        "saved account kept"
    );
    assert!(live_files.join("outgoing/batch/item/photo.jpg").exists());
    assert!(removed_services.symlink_metadata().is_err(), "link removed");
    assert!(!removed_files.exists());
    assert_eq!(
        std::fs::read(outside.path().join("precious")).unwrap(),
        b"keep",
        "the link target is untouched"
    );
    assert!(restored.account_purges.is_empty());
    assert!(saved(&restored).await.account_purges.is_empty());
}
