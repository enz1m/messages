use super::*;
use messages_protocols::{AdapterError, Registry};

#[tokio::test]
async fn background_sync_retries_transient_failures_quietly_without_touching_warnings_or_sends() {
    let server = TestServer::start().await.unwrap();
    let directory = tempfile::tempdir().unwrap();
    for (protocol, index, native_bridge) in [
        ("signal", 0, false),
        ("matrix", 1, false),
        ("whatsapp", 3, true),
    ] {
        let mut app = app_in(directory.path());
        let id = fixture::account_id(index);
        app.store.accounts.retain(|a| a.id == id);
        app.store.accounts[0].user_id.clear();
        app.selected = None;
        app.notice = Some("Retain this storage warning".into());
        let config = ConnectionConfig {
            account: id,
            protocol: ProtocolId::new(protocol),
            label: protocol.into(),
            endpoint: format!("{}/{protocol}", server.url),
            user_id: "you".into(),
            rooms: Vec::new(),
            bot: false,
            native_bridge,
            managed: None,
        };
        app.adapters.insert(
            id,
            Registry::default()
                .connect(config, Some("test-token".into()))
                .unwrap(),
        );
        server.state.lock().unwrap().next_failure = Some(503);
        let task = app.sync_account(id);
        drive(&mut app, task).await;
        assert!(!app.sync_due(id));
        assert!(outputs(app.sync_account(id)).await.is_empty());
        assert_eq!(
            app.notice.as_ref().unwrap().text,
            "Retain this storage warning"
        );
        app.sync_retry.get_mut(&id).unwrap().due = std::time::Instant::now();
        let task = app.sync_account(id);
        drive(&mut app, task).await;
        assert!(app.sync_due(id));
        assert!(!app.sync_retry.contains_key(&id));
        assert_eq!(
            app.notice.as_ref().unwrap().text,
            "Retain this storage warning"
        );
        if protocol == "whatsapp" {
            server
                .state
                .lock()
                .unwrap()
                .login_steps
                .insert(protocol.into(), "connecting".into());
            let task = app.sync_account(id);
            drive(&mut app, task).await;
            assert!(!app.sync_due(id));
            assert!(matches!(
                app.store.accounts[0].connection,
                ConnectionState::Connecting
            ));
            assert_eq!(
                app.notice.as_ref().unwrap().text,
                "Retain this storage warning"
            );
            server.state.lock().unwrap().login_steps.remove(protocol);
        }
        let _ = app.update(Message::Synced(
            id,
            Err(AdapterError::RateLimited(Duration::from_secs(30))),
        ));
        assert!(!app.sync_due(id));
        let _ = app.update(Message::Synced(id, Err(AdapterError::Authentication)));
        assert!(matches!(
            app.store.accounts[0].connection,
            ConnectionState::NeedsAuthentication(_)
        ));
        assert!(app.notice.as_ref().unwrap().text.contains("Sign in again"));
    }
    assert!(server.state.lock().unwrap().requests.iter().all(|r| {
        r.method == "GET"
            || matches!(
                r.body["method"].as_str(),
                Some("listContacts" | "listGroups")
            )
    }));
}

#[tokio::test]
async fn avatar_cache_survives_restart_is_private_and_keeps_last_good_photo_on_failed_refresh() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = app_in(directory.path());
    let key = app.store.conversations[0].key.clone();
    let session = app.session(key.account);
    let image = iced::widget::image::Handle::from_rgba(2, 2, vec![200; 16]);
    let task = app.update(Message::AvatarReady(
        session,
        key.clone(),
        0,
        Ok(Some(image)),
    ));
    drive(&mut app, task).await;
    let photos = crate::platform::avatar_cache::load(app.storage_path.clone(), &app.store).await;
    assert_eq!(photos.len(), 1);
    let mut reopened = app_in(directory.path());
    reopened.avatars.restore(photos);
    reopened.renew_session(key.account);
    assert!(reopened.avatar_image(&key).is_some());
    let account = reopened
        .store
        .accounts
        .iter_mut()
        .find(|a| a.id == key.account)
        .unwrap();
    let owner = std::mem::replace(&mut account.user_id, "different-owner".into());
    assert!(reopened.avatar_image(&key).is_none());
    assert!(
        crate::platform::avatar_cache::load(reopened.storage_path.clone(), &reopened.store)
            .await
            .is_empty()
    );
    reopened
        .store
        .accounts
        .iter_mut()
        .find(|a| a.id == key.account)
        .unwrap()
        .user_id = owner;
    let session = reopened.session(key.account);
    let task = reopened.update(Message::AvatarReady(session, key.clone(), 0, Err(())));
    drive(&mut reopened, task).await;
    assert!(reopened.avatar_image(&key).is_some());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(directory.path().join("avatar-cache.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0);
    }
    reopened.store.room_mut(&key).unwrap().avatar_revision = 1;
    assert!(reopened.avatar_image(&key).is_none());
    reopened.avatars.forget(key.account);
    let task = reopened.save_avatars();
    drive(&mut reopened, task).await;
    assert!(
        crate::platform::avatar_cache::load(reopened.storage_path.clone(), &reopened.store)
            .await
            .is_empty()
    );
    let _ = reopened.avatar_ready(
        app.session(key.account),
        key.clone(),
        0,
        Ok(Some(iced::widget::image::Handle::from_rgba(
            1,
            1,
            vec![0; 4],
        ))),
    );
    assert!(reopened.avatar_image(&key).is_none());
}
