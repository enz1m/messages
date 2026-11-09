use super::*;
use messages_protocols::login::LoginState;

#[tokio::test]
async fn matrix_recovery_masks_input_preserves_session_and_rejects_stale_completion() {
    let directory = tempfile::tempdir().unwrap();
    let server = TestServer::start().await.unwrap();
    let mut app = matrix_app(directory.path(), &server).await;
    let account = app.store.accounts[0].id;
    let task = app.update(Message::MatrixRecovery(account));
    drive(&mut app, task).await;
    assert!(app.login.is_none());
    app.connections[0].native_bridge = true;
    let config = app.connections[0].clone();
    let mut remote = config.clone();
    remote.endpoint = "https://helper.example.invalid".into();
    let remote = app.registry.connect(remote, None).unwrap();
    assert!(!remote.supports_encryption_recovery());
    assert!(matches!(
        remote
            .authenticate(messages_protocols::login::LoginInput {
                user: String::new(),
                value: "synthetic-recovery-secret".to_owned().into(),
                recover: true
            })
            .await,
        Err(messages_protocols::AdapterError::Unsupported(_))
    ));
    app.adapters.insert(
        account,
        app.registry
            .connect(config, Some("test-token".into()))
            .unwrap(),
    );
    let session = app.session(account);
    let task = app.update(Message::MatrixRecovery(account));
    drive(&mut app, task).await;
    assert!(matches!(
        app.login.as_ref().unwrap().state,
        LoginState::MatrixRecovery
    ));
    let task = app.update(Message::LoginLoad(account));
    drive(&mut app, task).await;
    assert!(matches!(
        app.login.as_ref().unwrap().state,
        LoginState::MatrixRecovery
    ));
    let secret = Message::LoginValue("synthetic-recovery-secret".to_owned().into());
    assert!(!format!("{secret:?}").contains("synthetic-recovery-secret"));
    let _ = app.update(secret);
    let task = app.update(Message::LoginSubmit);
    assert!(app.login.as_ref().unwrap().value.is_empty());
    let delayed = outputs(task).await;
    let task = app.update(Message::CloseDialog);
    drive(&mut app, task).await;
    let task = app.update(Message::MatrixRecovery(account));
    drive(&mut app, task).await;
    let showing = app.login.as_ref().unwrap().showing;
    super::requests::complete(&mut app, delayed).await;
    assert_eq!(app.login.as_ref().unwrap().showing, showing);
    let _ = app.update(Message::LoginValue(
        "synthetic-invalid-recovery".to_owned().into(),
    ));
    let task = app.update(Message::LoginSubmit);
    drive(&mut app, task).await;
    assert!(
        app.login
            .as_ref()
            .unwrap()
            .error
            .as_ref()
            .unwrap()
            .contains("recovery key or passphrase")
    );
    assert!(app.login.as_ref().unwrap().value.is_empty());
    let _ = app.update(Message::LoginValue(
        "synthetic-recovery-secret".to_owned().into(),
    ));
    let task = app.update(Message::LoginSubmit);
    drive(&mut app, task).await;
    assert!(app.login.is_none());
    assert!(app.session_is_current(session));
    let state = server.state.lock().unwrap();
    assert!(state.requests.iter().any(|r| r.method == "POST"
        && r.path.ends_with("/v1/login")
        && r.body["operation"] == "recover"));
    assert!(
        !std::fs::read_to_string(app.snapshot_path())
            .unwrap_or_default()
            .contains("synthetic-recovery-secret")
    );
}

#[tokio::test]
async fn login_closed_browser_attempt_cancels_original_adapter_and_discards_late_url() {
    let directory = tempfile::tempdir().unwrap();
    let server = TestServer::start().await.unwrap();
    let mut app = matrix_app(directory.path(), &server).await;
    let account = app.store.accounts[0].id;
    app.connections[0].native_bridge = true;
    let config = app.connections[0].clone();
    app.adapters.insert(
        account,
        app.registry
            .connect(config, Some("test-token".into()))
            .unwrap(),
    );
    let task = app.update(Message::LoginLoad(account));
    drive(&mut app, task).await;
    assert!(matches!(
        app.login.as_ref().unwrap().state,
        LoginState::Matrix { browser: true, .. }
    ));
    let mut expected_cancellations = Vec::new();
    for (code, expected) in [
        (
            "registration_rejected",
            messages_protocols::login::BrowserFailure::RegistrationRejected,
        ),
        (
            "unknown-fixture-secret",
            messages_protocols::login::BrowserFailure::StartFailed,
        ),
    ] {
        server.state.lock().unwrap().browser_failure = Some(serde_json::json!({
            "error_code":code,"error_description":"fixture-secret-access-token"
        }));
        let task = app.update(Message::BrowserBegin);
        expected_cancellations.push(
            app.login
                .as_ref()
                .unwrap()
                .browser
                .as_ref()
                .unwrap()
                .request,
        );
        drive(&mut app, task).await;
        let login = app.login.as_ref().unwrap();
        assert_eq!(login.error.as_deref(), Some(expected.to_string().as_str()));
        assert!(!login.error.as_ref().unwrap().contains("fixture-secret"));
        assert!(login.browser.is_none());
        assert!(!login.busy);
    }
    server.state.lock().unwrap().browser_failure = None;
    let task = app.update(Message::BrowserBegin);
    let delayed = outputs(task).await;
    let original = app
        .login
        .as_ref()
        .unwrap()
        .browser
        .as_ref()
        .unwrap()
        .request;
    let task = app.update(Message::CloseDialog);
    app.jobs.cancel_network();
    assert!(
        app.jobs
            .live()
            .iter()
            .any(|(_, kind)| *kind == super::super::jobs::Kind::AuthenticationCleanup)
    );
    drive(&mut app, task).await;
    let task = app.update(Message::LoginLoad(account));
    drive(&mut app, task).await;
    let showing = app.login.as_ref().unwrap().showing;
    super::requests::complete(&mut app, delayed).await;
    assert_eq!(app.login.as_ref().unwrap().showing, showing);
    assert!(app.login.as_ref().unwrap().browser.is_none());
    assert!(matches!(
        app.login.as_ref().unwrap().state,
        LoginState::Matrix { .. }
    ));
    let task = app.update(Message::BrowserBegin);
    let retired = app
        .login
        .as_ref()
        .unwrap()
        .browser
        .as_ref()
        .unwrap()
        .request;
    let delayed_after_reconnect = outputs(task).await;
    app.renew_session(account);
    let task = app.update(Message::BrowserOpen);
    drive(&mut app, task).await;
    assert!(app.login.is_none());
    assert!(app.dialog.is_none());
    super::requests::complete(&mut app, delayed_after_reconnect).await;
    let state = server.state.lock().unwrap();
    let cancellations: Vec<_> = state
        .requests
        .iter()
        .filter(|r| r.method == "DELETE" && r.path.ends_with("/v1/browser"))
        .collect();
    assert!(!cancellations.is_empty());
    expected_cancellations.extend([original, retired]);
    assert!(
        cancellations
            .iter()
            .all(|r| expected_cancellations.contains(
                &serde_json::from_value::<uuid::Uuid>(r.body["request_id"].clone()).unwrap()
            ))
    );
}
