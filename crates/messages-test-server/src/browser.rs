use crate::{MockState, reply};
use axum::response::Response;
use messages_protocols::bridge::{API_VERSION, BridgeStatus};
use serde_json::{Value, json};

pub(super) fn handle(state: &mut MockState, method: &str, route: &[&str], body: Value) -> Response {
    match (method, route) {
        ("GET", ["v1", "status"]) => reply(
            200,
            json!(BridgeStatus {
                api_version: API_VERSION,
                protocol: "matrix".into(),
                authenticated: false,
                user_id: None,
                capabilities: messages_core::fixture::sample_capabilities("matrix"),
                login_state: None,
            }),
        ),
        ("GET", ["v1", "login"]) => reply(
            200,
            json!({"state":"matrix","password":true,"browser":true}),
        ),
        ("POST", ["v1", "browser"]) => {
            if let Some(failure) = state.browser_failure.clone() {
                return reply(400, failure);
            }
            let request = &body["request_id"];
            reply(
                200,
                json!({"request_id":request,"url":"https://example.invalid/synthetic-sign-in","expires_in":600}),
            )
        }
        ("DELETE", ["v1", "browser"]) => reply(200, json!({"ok":true})),
        ("POST", ["v1", "login"]) if body["operation"] == "recover" => {
            if body["value"] != "synthetic-recovery-secret" {
                return reply(400, json!({"error":"Invalid synthetic recovery input"}));
            }
            reply(200, json!({"ok":true}))
        }
        _ => reply(404, json!({"error":"unknown browser fixture route"})),
    }
}
