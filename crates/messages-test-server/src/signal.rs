use axum::response::{IntoResponse, Response};
use messages_core::fixture;
use serde_json::{Value, json};

use crate::{MockState, insert_message, reply};

pub(super) fn handle(
    state: &mut MockState,
    method: &str,
    route: &[&str],
    last_event: Option<&str>,
    body: Value,
) -> Response {
    match (method, route) {
        ("GET", ["api", "v1", "check"]) => reply(200, json!({"status":"ok"})),
        ("GET", ["api", "v1", "events"]) => {
            if let Some(chunks) = &state.signal_chunks {
                use futures_util::StreamExt;
                let stream =
                    futures_util::stream::iter(chunks.clone()).then(|(delay, bytes)| async move {
                        if delay > 0 {
                            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                        }
                        Ok::<_, std::convert::Infallible>(bytes)
                    });
                return (
                    [("content-type", "text/event-stream")],
                    axum::body::Body::from_stream(stream),
                )
                    .into_response();
            }
            let skip = last_event
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(0);
            let mut events = String::new();
            let mut sequence = 0;
            for room in state
                .store
                .conversations
                .iter()
                .filter(|room| room.key.account == fixture::account_id(0))
            {
                for message in &room.messages {
                    sequence += 1;
                    if sequence <= skip {
                        continue;
                    }
                    let timestamp = message.timestamp.timestamp_millis();
                    let attachments: Vec<_> = message.attachments.iter().map(|file| json!({
                        "id":file.id,"filename":file.name,"contentType":file.mime,"size":file.bytes
                    })).collect();
                    let value = if message.own {
                        json!({"jsonrpc":"2.0","method":"receive","params":{"account":"you","envelope":{
                            "source":"you","sourceName":"You","syncMessage":{"sentMessage":{
                                "destination":room.key.room,"timestamp":timestamp,"message":message.body,"attachments":attachments}}}}})
                    } else {
                        json!({"jsonrpc":"2.0","method":"receive","params":{"account":"you","envelope":{
                            "source":room.key.room,"sourceNumber":room.key.room,"sourceName":message.sender_name,
                            "timestamp":timestamp,"dataMessage":{"timestamp":timestamp,"message":message.body,"attachments":attachments}}}})
                    };
                    events.push_str(&format!(
                        "id: {sequence}\nevent: receive\ndata: {value}\n\n"
                    ));
                }
            }
            ([("content-type", "text/event-stream")], events).into_response()
        }
        ("POST", ["api", "v1", "rpc"]) => {
            let method = body["method"].as_str().unwrap_or("");
            let params = &body["params"];
            let result = match method {
                "listAccounts" => json!(state.signal_account.iter().collect::<Vec<_>>()),
                "startLink" => {
                    json!({"deviceLinkUri":"sgnl://linkdevice?uuid=fixture&pub_key=fixture"})
                }
                "finishLink" => {
                    if params["deviceLinkUri"] != "sgnl://linkdevice?uuid=fixture&pub_key=fixture"
                        || params["deviceName"] != "Messages"
                    {
                        return reply(400, json!({"error":"invalid pairing request"}));
                    }
                    if let Some((account, error)) = state.signal_link_failure.take() {
                        state.signal_account = account;
                        return reply(200, json!({"jsonrpc":"2.0","id":body["id"],"error":error}));
                    }
                    let account = json!({"number":null,"aci":"fixture-linked-aci"});
                    state.signal_account = Some(account.clone());
                    account
                }

                "getUserStatus" => {
                    if params["username"].is_array() {
                        json!(params["username"].as_array().into_iter().flatten().filter_map(Value::as_str)
                            .map(|username| json!({"recipient":username,"number":null,"uuid":"fixture-username-aci","isRegistered":username != "missing.123"})).collect::<Vec<_>>())
                    } else {
                        json!(params["recipient"].as_array().into_iter().flatten().filter_map(Value::as_str)
                            .map(|recipient| json!({"recipient":recipient,"number":recipient,"isRegistered":recipient != "+15550109999"})).collect::<Vec<_>>())
                    }
                }
                "listContacts" => json!(
                    state
                        .store
                        .conversations
                        .iter()
                        .filter(|room| room.key.account == fixture::account_id(0))
                        .map(|room| json!({"number":room.key.room,"name":room.title}))
                        .collect::<Vec<_>>()
                ),
                "listGroups" => json!([]),
                "getAttachment" => {
                    json!({"data":state.signal_attachment_data.as_deref().unwrap_or("SGVsbG8gU2lnbmFsIQ==")})
                }
                "send" => {
                    let room = params["recipient"][0]
                        .as_str()
                        .or(params["recipient"].as_str())
                        .unwrap_or("");
                    if let Some(timestamp) = params["editTimestamp"].as_i64() {
                        let target = state
                            .store
                            .conversations
                            .iter_mut()
                            .find(|entry| {
                                entry.key.room == room
                                    && entry.key.account == fixture::account_id(0)
                            })
                            .and_then(|entry| {
                                entry.messages.iter_mut().find(|message| {
                                    message.own && message.timestamp.timestamp_millis() == timestamp
                                })
                            });
                        let Some(target) = target else {
                            return reply(
                                200,
                                json!({"jsonrpc":"2.0","id":body["id"],"error":{"code":-1,"message":"edit target not found"}}),
                            );
                        };
                        target.body = params["message"].as_str().unwrap_or("").into();
                        target.edited = true;
                        return reply(
                            200,
                            json!({"jsonrpc":"2.0","id":body["id"],"result":{"timestamp":chrono::Utc::now().timestamp_millis()}}),
                        );
                    }
                    let Some(message) = insert_message(
                        state,
                        0,
                        room,
                        params["message"].as_str().unwrap_or(""),
                        None,
                        None,
                    ) else {
                        return reply(
                            200,
                            json!({"jsonrpc":"2.0","id":body["id"],"error":{"code":-1,"message":"recipient not found"}}),
                        );
                    };
                    json!({"timestamp":message.timestamp.timestamp_millis()})
                }
                "sendReceipt" if !params["recipient"].is_string() => {
                    return reply(
                        200,
                        json!({"jsonrpc":"2.0","id":body["id"],"error":{"code":-32602,"message":"recipient must be a string"}}),
                    );
                }
                "sendReaction" | "remoteDelete" | "sendReceipt" | "sendTyping" => {
                    json!({"timestamp":chrono::Utc::now().timestamp_millis()})
                }
                _ => {
                    return reply(
                        200,
                        json!({"jsonrpc":"2.0","id":body["id"],"error":{"code":-32601,"message":"unknown method"}}),
                    );
                }
            };
            reply(
                200,
                json!({"jsonrpc":"2.0","id":body["id"],"result":result}),
            )
        }
        _ => reply(404, json!({"error":"unknown Signal fixture route"})),
    }
}
