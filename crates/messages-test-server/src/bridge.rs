use axum::response::Response;
use messages_core::fixture;
use messages_protocols::bridge::{API_VERSION, BridgeStatus};
use serde_json::{Value, json};

use crate::{MockState, insert_message, reply};

pub(super) fn handle(
    state: &mut MockState,
    protocol: &str,
    method: &str,
    route: &[&str],
    query: &str,
    body: Value,
) -> Response {
    let account = 3;
    let key = messages_core::RoomKey::new(
        fixture::account_id(account),
        body["chat_id"].as_str().unwrap_or(""),
    );
    match (method, route) {
        ("GET", ["v1", "avatar"]) => {
            use axum::response::IntoResponse;
            let chat = url::form_urlencoded::parse(query.as_bytes())
                .find(|(k, _)| k == "chat_id")
                .map(|(_, v)| v.into_owned())
                .unwrap_or_default();
            match state.avatar_data.get(&chat) {
                Some(data) => ([("content-type", "image/png")], data.clone()).into_response(),
                None => reply(404, json!({})),
            }
        }

        ("POST", ["v1", "call"]) if protocol == "whatsapp" => super::calls::handle(state, body),
        ("POST", ["v1", "open"]) => {
            let recipient = body["chat_id"].as_str().unwrap_or("");
            if recipient.is_empty() {
                return reply(400, json!({"error":"missing recipient"}));
            }
            if state.store.room(&key).is_none() {
                state
                    .store
                    .conversations
                    .push(messages_core::Conversation::empty(
                        key.clone(),
                        recipient,
                        messages_core::RoomKind::Direct,
                        messages_core::Encryption::Sample,
                        fixture::sample_capabilities(protocol),
                    ));
            }
            reply(200, json!({"conversation":state.store.room(&key)}))
        }
        ("GET", ["v1", "status"]) => reply(
            200,
            json!(BridgeStatus {
                api_version: API_VERSION,
                protocol: protocol.into(),
                authenticated: state
                    .login_steps
                    .get(protocol)
                    .is_none_or(|step| step == "ready"),
                user_id: Some("you".into()),
                capabilities: if state.calls_enabled && protocol == "whatsapp" {
                    super::calls::capabilities(&fixture::sample_capabilities(protocol))
                } else {
                    fixture::sample_capabilities(protocol)
                },
                login_state: None,
            }),
        ),
        ("GET", ["v1", "login"]) => {
            let step = state
                .login_steps
                .get(protocol)
                .map(String::as_str)
                .unwrap_or("ready");
            reply(
                200,
                if step == "qr" {
                    json!({"state":"qr","qr":"messages-account-free-pairing-fixture","expires_in":30})
                } else {
                    json!({"state":step})
                },
            )
        }
        ("POST", ["v1", "login"]) => {
            if body["value"].as_str().unwrap_or("").is_empty() {
                return reply(400, json!({"error":"missing input"}));
            }
            state.login_steps.insert(protocol.into(), "ready".into());
            reply(200, json!({"ok":true}))
        }
        ("GET", ["v1", "chats"]) => reply(
            200,
            json!({"conversations":state.store.conversations.iter()
            .filter(|room| room.key.account == fixture::account_id(account)).collect::<Vec<_>>(),"cursor":state.sequence.to_string()}),
        ),
        ("POST", ["v1", "history"]) => {
            if let Some(room) = state.store.room(&key) {
                let end = body["before"]
                    .as_str()
                    .and_then(|id| room.messages.iter().position(|message| message.id == id))
                    .unwrap_or(room.messages.len());
                let start = end.saturating_sub(50);
                reply(
                    200,
                    json!({"messages":&room.messages[start..end],"next":(start>0).then(|| room.messages[start].id.clone())}),
                )
            } else {
                reply(404, json!({}))
            }
        }
        ("POST", ["v1", "send"]) => {
            let transaction = body["transaction_id"]
                .as_str()
                .and_then(|value| uuid::Uuid::parse_str(value).ok());
            let Some(message) = insert_message(
                state,
                account,
                &key.room,
                body["body"].as_str().unwrap_or(""),
                transaction,
                body["reply_to"].as_str().map(str::to_owned),
            ) else {
                return reply(404, json!({}));
            };
            reply(200, json!({"message_id":message.id}))
        }
        ("POST", ["v1", operation @ ("react" | "edit" | "delete")]) => {
            let id = body["message_id"].as_str().unwrap_or("");
            let Some(message) = state
                .store
                .room_mut(&key)
                .and_then(|room| room.messages.iter_mut().find(|message| message.id == id))
            else {
                return reply(404, json!({}));
            };
            match *operation {
                "react" => {
                    let emoji = body["emoji"].as_str().unwrap_or("");
                    if body["remove"] == true {
                        message.reactions.retain(|reaction| reaction.emoji != emoji);
                    } else {
                        message.reactions.push(messages_core::Reaction {
                            emoji: emoji.into(),
                            count: 1,
                            own: true,
                        });
                    }
                }
                "edit" => {
                    message.body = body["body"].as_str().unwrap_or("").into();
                    message.edited = true;
                }
                "delete" => {
                    message.deleted = true;
                    message.body.clear();
                }
                _ => unreachable!(),
            }
            reply(200, json!({"ok":true}))
        }
        ("POST", ["v1", "read" | "typing"]) => reply(200, json!({"ok":true})),
        _ => reply(404, json!({"error":"unknown bridge fixture route"})),
    }
}
