use axum::response::Response;
use messages_core::fixture;
use serde_json::{Value, json};

use crate::{MockState, insert_message, reply};

fn event(message: &messages_core::ChatMessage) -> Value {
    let sender = if message.own {
        "@you:example.test".to_owned()
    } else if message.sender.starts_with('@') {
        message.sender.clone()
    } else {
        format!("@{}:example.test", message.sender)
    };
    json!({"type":"m.room.message","event_id":message.id,"sender":sender,
        "origin_server_ts":message.timestamp.timestamp_millis(),
        "content":{"msgtype":"m.text","body":message.body},
        "unsigned":{"transaction_id":message.transaction.map(|id| id.to_string())}})
}

fn seed_events(state: &mut MockState, room: &str) {
    if !state.matrix_events.contains_key(room) {
        let events = state
            .store
            .room(&messages_core::RoomKey::new(fixture::account_id(1), room))
            .map(|room| room.messages.iter().map(event).collect())
            .unwrap_or_default();
        state.matrix_events.insert(room.into(), events);
    }
}

fn action_event(state: &mut MockState, room: &str, kind: &str, content: Value) -> String {
    state.sequence += 1;
    let id = format!("$action-{}:example.test", state.sequence);
    state.matrix_events.entry(room.into()).or_default().push(
        json!({"event_id":id,"type":kind,"sender":"@you:example.test",
        "origin_server_ts":chrono::Utc::now().timestamp_millis(),"content":content}),
    );
    id
}

pub(super) fn handle(
    state: &mut MockState,
    method: &str,
    route: &[&str],
    query: &str,
    body: Value,
) -> Response {
    let ["_matrix", "client", "v3", tail @ ..] = route else {
        return reply(404, json!({}));
    };
    match (method, tail) {
        ("POST", ["createRoom"]) => {
            state.sequence += 1;
            let id = format!("!created-{}:example.test", state.sequence);
            let room = messages_core::Conversation::empty(
                messages_core::RoomKey::new(fixture::account_id(1), &id),
                body["invite"][0].as_str().unwrap_or("New room"),
                messages_core::RoomKind::Direct,
                messages_core::Encryption::Transport,
                fixture::sample_capabilities("matrix"),
            );
            state.store.conversations.push(room);
            reply(200, json!({"room_id":id}))
        }
        ("POST", ["join", id]) => {
            let id = if *id == "#rust:example.test" {
                "!rust:example.test"
            } else {
                *id
            };
            if state
                .store
                .room(&messages_core::RoomKey::new(fixture::account_id(1), id))
                .is_some()
            {
                reply(200, json!({"room_id":id}))
            } else {
                reply(404, json!({"errcode":"M_NOT_FOUND"}))
            }
        }
        ("GET", ["sync"]) => {
            let rooms: serde_json::Map<String, Value> = state.store.conversations.iter()
                .filter(|room| room.key.account == fixture::account_id(1)).map(|room| {
                    let events = state.matrix_events.get(&room.key.room).cloned().unwrap_or_else(|| room.messages.iter().map(event).collect::<Vec<_>>());
                    let start = events.len().saturating_sub(20);
                    (room.key.room.clone(), json!({
                    "state":{"events":[{"type":"m.room.name","content":{"name":room.title}}]},
                    "timeline":{"events":&events[start..],"limited":start>0,"prev_batch":format!("offset:{start}")},
                    "unread_notifications":{"notification_count":room.unread}
                }))}).collect();
            reply(
                200,
                json!({"next_batch":format!("sync-{}",state.sequence),"rooms":{"join":rooms}}),
            )
        }
        ("GET", ["rooms", room, "messages"]) => {
            let key = messages_core::RoomKey::new(fixture::account_id(1), *room);
            let Some(room) = state.store.room(&key) else {
                return reply(404, json!({}));
            };
            let events = state
                .matrix_events
                .get(&key.room)
                .cloned()
                .unwrap_or_else(|| room.messages.iter().map(event).collect());
            let from = url::form_urlencoded::parse(query.as_bytes())
                .find(|(name, _)| name == "from")
                .map(|(_, value)| value.into_owned());
            let end = match from {
                Some(from) => match from
                    .strip_prefix("offset:")
                    .and_then(|offset| offset.parse::<usize>().ok())
                {
                    Some(end) if end <= events.len() => end,
                    _ => return reply(400, json!({"errcode":"M_INVALID_PARAM"})),
                },
                None => events.len(),
            };
            let start = end.saturating_sub(50);
            reply(
                200,
                json!({"chunk":events[start..end].iter().rev().collect::<Vec<_>>(),"end":(start>0).then(|| format!("offset:{start}"))}),
            )
        }
        ("GET", ["rooms", id, "state", "m.room.encryption"]) => {
            let key = messages_core::RoomKey::new(fixture::account_id(1), *id);
            if state
                .store
                .room(&key)
                .is_some_and(|room| room.encryption == messages_core::Encryption::EndToEnd)
            {
                reply(200, json!({"algorithm":"m.megolm.v1.aes-sha2"}))
            } else {
                reply(404, json!({"errcode":"M_NOT_FOUND"}))
            }
        }
        ("PUT", ["rooms", room, "send", event_type, transaction]) => {
            let key = messages_core::RoomKey::new(fixture::account_id(1), *room);
            seed_events(state, room);
            if *event_type == "m.reaction" {
                let target = body["m.relates_to"]["event_id"].as_str().unwrap_or("");
                let emoji = body["m.relates_to"]["key"].as_str().unwrap_or("");
                if let Some(message) = state.store.room_mut(&key).and_then(|room| {
                    room.messages
                        .iter_mut()
                        .find(|message| message.id == target)
                }) {
                    message.reactions.push(messages_core::Reaction {
                        emoji: emoji.into(),
                        count: 1,
                        own: true,
                    });
                } else {
                    return reply(404, json!({}));
                }
                let id = action_event(state, room, "m.reaction", body);
                return reply(200, json!({"event_id":id}));
            }
            if body["m.relates_to"]["rel_type"] == "m.replace" {
                let target = body["m.relates_to"]["event_id"].as_str().unwrap_or("");
                if let Some(message) = state.store.room_mut(&key).and_then(|room| {
                    room.messages
                        .iter_mut()
                        .find(|message| message.id == target)
                }) {
                    message.body = body["m.new_content"]["body"].as_str().unwrap_or("").into();
                    message.edited = true;
                    let id = action_event(state, room, "m.room.message", body);
                    return reply(200, json!({"event_id":id}));
                }
                return reply(404, json!({}));
            }
            let transaction = uuid::Uuid::parse_str(transaction).ok();
            if let Some(existing) = state.store.room(&key).and_then(|room| {
                room.messages
                    .iter()
                    .find(|message| transaction.is_some() && message.transaction == transaction)
            }) {
                return reply(200, json!({"event_id":existing.id}));
            }
            let Some(message) = insert_message(
                state,
                1,
                room,
                body["body"].as_str().unwrap_or(""),
                transaction,
                body["m.relates_to"]["m.in_reply_to"]["event_id"]
                    .as_str()
                    .map(str::to_owned),
            ) else {
                return reply(404, json!({}));
            };
            state
                .matrix_events
                .entry((*room).into())
                .or_default()
                .push(event(&message));
            reply(200, json!({"event_id":message.id}))
        }
        ("PUT", ["rooms", room, "redact", id, _]) => {
            let key = messages_core::RoomKey::new(fixture::account_id(1), *room);
            seed_events(state, room);
            if state
                .matrix_events
                .get(*room)
                .is_some_and(|events| events.iter().any(|event| event["event_id"] == *id))
            {
                if let Some(message) = state
                    .store
                    .room_mut(&key)
                    .and_then(|room| room.messages.iter_mut().find(|message| message.id == *id))
                {
                    message.deleted = true;
                    message.body.clear();
                }
                let id = action_event(state, room, "m.room.redaction", json!({"redacts":id}));
                return reply(200, json!({"event_id":id}));
            }
            if let Some(message) = state
                .store
                .room_mut(&key)
                .and_then(|room| room.messages.iter_mut().find(|message| message.id == *id))
            {
                message.deleted = true;
                message.body.clear();
                reply(200, json!({"event_id":"redacted"}))
            } else {
                reply(404, json!({}))
            }
        }
        ("POST", ["rooms", _, "receipt", "m.read", _]) | ("PUT", ["rooms", _, "typing", _]) => {
            reply(200, json!({}))
        }
        _ => reply(404, json!({"error":"unknown Matrix fixture route"})),
    }
}
