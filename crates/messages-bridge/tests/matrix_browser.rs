#![cfg(feature = "matrix")]

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    response::IntoResponse,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use messages_bridge::{Backend, matrix::Matrix};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use url::Url;
use uuid::Uuid;

#[derive(Default)]
struct Observed {
    registered: usize,
    exchanged: usize,
    refreshed: usize,
    challenge: String,
    scope: String,
    redirect: String,
    login_tokens: std::collections::HashSet<String>,
}
struct Authority {
    url: String,
    observed: Arc<Mutex<Observed>>,
    expire: Arc<AtomicBool>,
    legacy: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Authority {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Authority {
    async fn start(oauth: bool) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let observed = Arc::new(Mutex::new(Observed::default()));
        let expire = Arc::new(AtomicBool::new(false));
        let legacy = Arc::new(AtomicBool::new(true));
        let legacy_route = legacy.clone();
        let origin = url.clone();
        let state = observed.clone();
        let expired = expire.clone();
        let router=Router::new().fallback(move |request:Request<Body>| {
            let origin=origin.clone();let observed=state.clone();let expire=expired.clone();let legacy=legacy_route.clone();
            async move {
                let uri=Url::parse(&format!("{}{}",origin,request.uri())).unwrap();let path=uri.path().to_owned();
                let authorization=request.headers().get("authorization").and_then(|v|v.to_str().ok()).unwrap_or("").to_owned();
                let method=request.method().as_str().to_owned();let bytes=to_bytes(request.into_body(),65536).await.unwrap();
                let value:Value=serde_json::from_slice(&bytes).unwrap_or(Value::Null);
                if path.ends_with("/versions") { return axum::Json(json!({"versions":["v1.1","v1.15"]})).into_response(); }
                if path.ends_with("/auth_metadata") {
                    return if oauth { axum::Json(json!({
                        "issuer":origin,"authorization_endpoint":format!("{origin}/authorize"),
                        "token_endpoint":format!("{origin}/token"),"registration_endpoint":format!("{origin}/register"),
                        "jwks_uri":format!("{origin}/jwks"),"revocation_endpoint":format!("{origin}/revoke"),"response_modes_supported":["query","fragment"],"account_management_actions_supported":[],"prompt_values_supported":[],"response_types_supported":["code"],
                        "grant_types_supported":["authorization_code","refresh_token"],"code_challenge_methods_supported":["S256"],
                        "token_endpoint_auth_methods_supported":["none"],"authorization_response_iss_parameter_supported":true
                    })).into_response() } else { (StatusCode::NOT_FOUND,axum::Json(json!({"errcode":"M_UNRECOGNIZED","error":"not supported"}))).into_response() };
                }
                if path=="/register" {
                    let client_uri=value["client_uri"].as_str().and_then(|v|Url::parse(v).ok());
                    if client_uri.as_ref().is_none_or(|uri|uri.scheme()!="https" || uri.host_str().is_none_or(|host|host=="localhost" || host.parse::<std::net::IpAddr>().is_ok())) {
                        return (StatusCode::BAD_REQUEST,axum::Json(json!({"error":"invalid_client_metadata","error_description":"invalid client_uri"}))).into_response();
                    }
                    assert_eq!(value["application_type"],"native");assert_eq!(value["token_endpoint_auth_method"],"none");
                    assert!(value.get("client_secret").is_none());observed.lock().unwrap().registered+=1;
                    return axum::Json(json!({"client_id":"messages-native-fixture"})).into_response();
                }
                if path=="/authorize" {
                    let query=uri.query_pairs().into_owned().collect::<std::collections::HashMap<_,_>>();
                    assert_eq!(query.get("code_challenge_method").map(String::as_str),Some("S256"));
                    let mut state=observed.lock().unwrap();state.challenge=query["code_challenge"].clone();state.scope=query["scope"].clone();state.redirect=query["redirect_uri"].clone();
                    let mut redirect=Url::parse(&state.redirect).unwrap();redirect.query_pairs_mut().append_pair("code","fixture-browser-code").append_pair("state",&query["state"]).append_pair("iss",&origin);
                    return (StatusCode::SEE_OTHER,[("location",redirect.as_str())]).into_response();
                }
                if path=="/token" {
                    let params=url::form_urlencoded::parse(&bytes).into_owned().collect::<std::collections::HashMap<_,_>>();
                    let mut state=observed.lock().unwrap();let refresh=params.get("grant_type").is_some_and(|v|v=="refresh_token");
                    if refresh { state.refreshed+=1;assert_eq!(params.get("refresh_token").map(String::as_str),Some("fixture-refresh")); }
                    else {
                        assert_eq!(params.get("code").map(String::as_str),Some("fixture-browser-code"));
                        assert_eq!(params.get("redirect_uri"),Some(&state.redirect));
                        let verifier=params.get("code_verifier").expect("PKCE verifier");
                        assert_eq!(URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),state.challenge);
                        state.exchanged+=1;
                    }
                    return axum::Json(json!({"access_token":if refresh {"fixture-refreshed-access"}else{"fixture-access"},"token_type":"Bearer","refresh_token":if refresh {"fixture-next-refresh"}else{"fixture-refresh"},"expires_in":3600,"scope":state.scope})).into_response();
                }
                if path.ends_with("/login/sso/redirect") {
                    let target=uri.query_pairs().find(|(key,_)|key=="redirectUrl").unwrap().1.into_owned();
                    let token=format!("fixture-token-{}",Uuid::new_v4());observed.lock().unwrap().login_tokens.insert(token.clone());
                    let mut redirect=Url::parse(&target).unwrap();redirect.query_pairs_mut().append_pair("loginToken",&token);
                    return (StatusCode::SEE_OTHER,[("location",redirect.as_str())]).into_response();
                }
                if path.ends_with("/login") {
                    if method=="GET" { return axum::Json(json!({"flows":if legacy.load(Ordering::Acquire) {vec![json!({"type":"m.login.sso"})]}else{Vec::<Value>::new()}})).into_response(); }
                    assert_eq!(value["type"],"m.login.token");let token=value["token"].as_str().unwrap();
                    let mut state=observed.lock().unwrap();assert!(state.login_tokens.remove(token));state.exchanged+=1;
                    return axum::Json(json!({"user_id":"@browser:example.test","device_id":"FIXTURE","access_token":"fixture-access","refresh_token":"fixture-refresh"})).into_response();
                }
                if path.ends_with("/account/whoami") { return axum::Json(json!({"user_id":"@browser:example.test","device_id":"FIXTURE","is_guest":false})).into_response(); }
                if path.ends_with("/sync") {
                    if expire.load(Ordering::Acquire)&&authorization=="Bearer fixture-access" { return (StatusCode::UNAUTHORIZED,axum::Json(json!({"errcode":"M_UNKNOWN_TOKEN","error":"expired"}))).into_response(); }
                    return axum::Json(json!({"next_batch":"fixture-cursor","rooms":{},"to_device":{"events":[]},"device_lists":{"changed":[],"left":[]}})).into_response();
                }
                axum::Json(json!({})).into_response()
            }
        });
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            url,
            observed,
            expire,
            legacy,
            task,
        }
    }
    async fn authorize(&self, url: &str) -> Url {
        let response = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap()
            .get(url)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        Url::parse(response.headers()["location"].to_str().unwrap()).unwrap()
    }
}

#[tokio::test]
async fn legacy_sso_rejects_foreign_host_path_and_duplicate_tokens_then_restores_private_session() {
    let authority = Authority::start(false).await;
    let directory = tempfile::tempdir().unwrap();
    let backend = Matrix::prepare(directory.path(), &authority.url)
        .await
        .unwrap();
    use std::os::unix::fs::PermissionsExt;
    let key_path = directory.path().join("database-key");
    assert_eq!(
        std::fs::metadata(&key_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let database_key = std::fs::read(&key_path).unwrap();
    let challenge = backend.browser_start(Uuid::new_v4()).await.unwrap();
    let callback = authority
        .authorize(challenge["url"].as_str().unwrap())
        .await;
    let http = reqwest::Client::new();
    let mut wrong = callback.clone();
    wrong.set_path("/foreign");
    assert_eq!(
        http.get(wrong).send().await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        http.get(callback.clone())
            .header("Host", "foreign.invalid")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    let mut duplicate = callback.clone();
    duplicate
        .query_pairs_mut()
        .append_pair("loginToken", "credential-canary");
    let rejected = http.get(duplicate).send().await.unwrap();
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
    assert!(!rejected.text().await.unwrap().contains("credential-canary"));
    assert_eq!(
        http.get(callback.clone()).send().await.unwrap().status(),
        StatusCode::OK
    );
    assert!(backend.status().await.authenticated);
    assert_eq!(authority.observed.lock().unwrap().exchanged, 1);
    let path = directory.path().join("session.json");
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    drop(backend);
    let restored = Matrix::prepare(directory.path(), &authority.url)
        .await
        .unwrap();
    assert!(restored.status().await.authenticated);
    assert_eq!(std::fs::read(&key_path).unwrap(), database_key);
    assert_eq!(
        restored.status().await.user_id.as_deref(),
        Some("@browser:example.test")
    );
    assert!(http.get(callback).send().await.is_err());

    let dual = Authority::start(true).await;
    let dual_directory = tempfile::tempdir().unwrap();
    let dual_backend = Matrix::prepare_with_client_uri(dual_directory.path(), &dual.url, Some(""))
        .await
        .unwrap();
    let dual_challenge = dual_backend.browser_start(Uuid::new_v4()).await.unwrap();
    let sso_url = dual_challenge["url"].as_str().unwrap();
    assert!(
        Url::parse(sso_url)
            .unwrap()
            .path()
            .ends_with("/login/sso/redirect")
    );
    let dual_callback = dual.authorize(sso_url).await;
    assert_eq!(
        http.get(dual_callback).send().await.unwrap().status(),
        StatusCode::OK
    );
    assert!(dual_backend.status().await.authenticated);
    assert!(dual_directory.path().join("session.json").exists());
    let observed = dual.observed.lock().unwrap();
    assert_eq!(observed.registered, 0);
    assert_eq!(observed.exchanged, 1);
}

#[tokio::test]
async fn oauth_browser_uses_pkce_rejects_wrong_state_and_persists_refreshed_session() {
    let authority = Authority::start(true).await;
    authority.legacy.store(false, Ordering::Release);
    for uri in [
        "",
        "http://192.168.1.10:8080/private",
        "https://localhost/app",
        "https://app.example.test/?token=fixture-secret",
    ] {
        let invalid_directory = tempfile::tempdir().unwrap();
        let invalid =
            Matrix::prepare_with_client_uri(invalid_directory.path(), &authority.url, Some(uri))
                .await
                .unwrap();
        let error = invalid.browser_start(Uuid::new_v4()).await.unwrap_err();
        assert!(matches!(
            error.downcast_ref::<messages_protocols::login::BrowserFailure>(),
            Some(messages_protocols::login::BrowserFailure::AppIdentityMissing)
        ));
        assert!(!error.to_string().contains("fixture-secret"));
        assert!(!invalid.status().await.authenticated);
        assert!(!invalid_directory.path().join("session.json").exists());
    }
    assert_eq!(authority.observed.lock().unwrap().registered, 0);
    let directory = tempfile::tempdir().unwrap();
    let backend = Matrix::prepare_with_client_uri(
        directory.path(),
        &authority.url,
        Some("https://messages.example.test"),
    )
    .await
    .unwrap();
    let methods = backend.login().await;
    assert_eq!(methods["browser"], true);
    let challenge = backend.browser_start(Uuid::new_v4()).await.unwrap();
    let callback = authority
        .authorize(challenge["url"].as_str().unwrap())
        .await;
    let mut wrong = callback.clone();
    let query = callback
        .query_pairs()
        .filter(|(key, _)| key != "state")
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect::<Vec<_>>();
    wrong.set_query(None);
    wrong
        .query_pairs_mut()
        .extend_pairs(query)
        .append_pair("state", "foreign-state");
    let http = reqwest::Client::new();
    assert_eq!(
        http.get(wrong).send().await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
    assert!(!backend.status().await.authenticated);
    assert_eq!(
        http.get(callback).send().await.unwrap().status(),
        StatusCode::OK
    );
    assert!(backend.status().await.authenticated);
    {
        let observed = authority.observed.lock().unwrap();
        assert_eq!(observed.registered, 1);
        assert_eq!(observed.exchanged, 1);
    }
    authority.expire.store(true, Ordering::Release);
    let (shutdown, receiver) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(backend.clone().run(receiver));
    let session_path = directory.path().join("session.json");
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if std::fs::read_to_string(&session_path)
                .unwrap()
                .contains("fixture-refreshed-access")
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("refreshed credentials must be durably saved");
    shutdown.send(true).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), worker)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    drop(backend);
    let restored = Matrix::prepare(directory.path(), &authority.url)
        .await
        .unwrap();
    assert!(restored.status().await.authenticated);
    assert_eq!(authority.observed.lock().unwrap().refreshed, 1);
}

#[tokio::test]
async fn browser_cancel_retires_early_delayed_and_replayed_requests_without_canceling_a_new_one() {
    let authority = Authority::start(false).await;
    let directory = tempfile::tempdir().unwrap();
    let backend = Matrix::prepare(directory.path(), &authority.url)
        .await
        .unwrap();
    let early = Uuid::new_v4();
    backend.browser_cancel(early).await.unwrap();
    assert!(backend.browser_start(early).await.is_err());
    let old = Uuid::new_v4();
    let first = backend.browser_start(old).await.unwrap();
    let callback = authority.authorize(first["url"].as_str().unwrap()).await;
    backend.browser_cancel(old).await.unwrap();
    assert!(reqwest::get(callback).await.is_err());
    assert!(backend.browser_start(old).await.is_err());
    let current = Uuid::new_v4();
    backend.browser_start(current).await.unwrap();
    backend.browser_cancel(old).await.unwrap();
    assert_eq!(backend.login().await["request_id"], current.to_string());
    assert!(!backend.status().await.authenticated);
    assert!(!directory.path().join("session.json").exists());
    assert_eq!(authority.observed.lock().unwrap().exchanged, 0);
    backend.browser_cancel(current).await.unwrap();
}
