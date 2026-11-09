use messages_core::{ProtocolId, fixture};
use messages_protocols::{ConnectionConfig, Registry, login::LoginState};
use messages_test_server::TestServer;

fn config(server: &TestServer) -> ConnectionConfig {
    ConnectionConfig {
        account: fixture::account_id(0),
        protocol: ProtocolId::new("signal"),
        label: "Signal".into(),
        endpoint: format!("{}/signal", server.url),
        user_id: String::new(),
        rooms: Vec::new(),
        bot: false,
        native_bridge: false,
        managed: None,
    }
}

#[tokio::test]
async fn signal_login_links_a_device_and_routes_numberless_accounts() {
    let server = TestServer::start().await.unwrap();
    server.state.lock().unwrap().signal_account = None;
    let adapter = Registry::default().connect(config(&server), None).unwrap();
    let login = adapter.login_state().await.unwrap();
    assert!(matches!(login, LoginState::Qr { .. }));
    assert!(!format!("{login:?}").contains("pub_key=fixture"));
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if matches!(adapter.login_state().await.unwrap(), LoginState::Ready) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let batch = adapter.sync(None).await.unwrap();
    assert_eq!(batch.user_id.as_deref(), Some("fixture-linked-aci"));
    {
        let state = server.state.lock().unwrap();
        assert_eq!(
            state
                .requests
                .iter()
                .filter(|request| request.body["method"] == "startLink")
                .count(),
            1
        );
        assert_eq!(
            state
                .requests
                .iter()
                .filter(|request| request.body["method"] == "finishLink")
                .count(),
            1
        );
        for request in state.requests.iter().filter(|request| {
            matches!(
                request.body["method"].as_str(),
                Some("listContacts" | "listGroups")
            )
        }) {
            assert_eq!(request.body["params"]["account"], "fixture-linked-aci");
        }
    }
    drop(adapter);
    let restored = Registry::default().connect(config(&server), None).unwrap();
    assert!(matches!(
        restored.login_state().await.unwrap(),
        LoginState::Ready
    ));
    assert_eq!(
        restored.sync(None).await.unwrap().user_id.as_deref(),
        Some("fixture-linked-aci")
    );
    assert_eq!(
        server
            .state
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|r| r.body["method"] == "startLink")
            .count(),
        1
    );
}

#[tokio::test]
async fn signal_login_reconciles_a_failed_finish_only_when_the_account_is_confirmed() {
    use messages_protocols::AdapterError;
    use serde_json::json;
    for (published, expected, ready) in [
        (
            Some(json!({"number":null,"aci":"fixture-linked-aci"})),
            "",
            true,
        ),
        (None, "", false),
        (
            Some(json!({"number":null,"aci":"unrelated-aci"})),
            "expected-aci",
            false,
        ),
    ] {
        let server = TestServer::start().await.unwrap();
        {
            let mut state = server.state.lock().unwrap();
            state.signal_account = None;
            state.signal_link_failure = Some((
                published,
                json!({"code":-32603,"message":"private-error-canary"}),
            ));
        }
        let mut config = config(&server);
        config.user_id = expected.into();
        let adapter = Registry::default().connect(config, None).unwrap();
        assert!(matches!(
            adapter.login_state().await.unwrap(),
            LoginState::Qr { .. }
        ));
        let settled = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                match adapter.login_state().await {
                    Ok(LoginState::Qr { .. }) => tokio::task::yield_now().await,
                    result => break result,
                }
            }
        })
        .await
        .unwrap();
        if ready {
            assert!(matches!(settled.unwrap(), LoginState::Ready));
            assert!(matches!(
                adapter.login_state().await.unwrap(),
                LoginState::Ready
            ));
        } else {
            let error = settled.unwrap_err();
            assert!(matches!(error, AdapterError::InvalidData(_)));
            assert!(!error.to_string().contains("private-error-canary"));
        }
        let state = server.state.lock().unwrap();
        for method in ["startLink", "finishLink"] {
            assert_eq!(
                state
                    .requests
                    .iter()
                    .filter(|request| request.body["method"] == method)
                    .count(),
                1
            );
        }
        assert_eq!(
            state
                .requests
                .iter()
                .filter(|request| request.body["method"] == "listAccounts")
                .count(),
            2
        );
    }
}
