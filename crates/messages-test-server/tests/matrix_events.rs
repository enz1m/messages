use messages_core::{RoomKey, fixture};
use messages_protocols::{ConnectionConfig, Registry};
use messages_test_server::TestServer;
use serde_json::{Value, json};

const ROOM: &str = "!rust:example.test";
const OWN: &str = "@you:example.test";

fn event(id: &str, kind: &str, sender: &str, time: i64, content: Value) -> Value {
    json!({"event_id":id,"type":kind,"sender":sender,"origin_server_ts":time,"content":content})
}

fn message() -> Value {
    event(
        "$base",
        "m.room.message",
        OWN,
        1000,
        json!({"body":"Original body","msgtype":"m.text"}),
    )
}
fn edit(id: &str, sender: &str, time: i64, body: &str) -> Value {
    event(
        id,
        "m.room.message",
        sender,
        time,
        json!({"body":format!("* {body}"),"m.new_content":{"body":body,"msgtype":"m.text"},
        "m.relates_to":{"rel_type":"m.replace","event_id":"$base"}}),
    )
}
fn reaction(id: &str, sender: &str) -> Value {
    event(
        id,
        "m.reaction",
        sender,
        2000,
        json!({"m.relates_to":{"rel_type":"m.annotation","event_id":"$base","key":"👍"}}),
    )
}
fn redact(id: &str, target: &str) -> Value {
    event(id, "m.room.redaction", OWN, 4000, json!({"redacts":target}))
}
fn config(server: &TestServer) -> ConnectionConfig {
    ConnectionConfig {
        account: fixture::account_id(1),
        protocol: messages_core::ProtocolId::new("matrix"),
        label: "Matrix".into(),
        endpoint: format!("{}/matrix", server.url),
        user_id: OWN.into(),
        rooms: Vec::new(),
        bot: false,
        native_bridge: false,
        managed: None,
    }
}

#[tokio::test]
async fn matrix_events_replay_relations_out_of_order_and_redact_without_resurrection() {
    let server = TestServer::start().await.unwrap();
    let events = vec![
        reaction("$own-reaction", OWN),
        edit("$edit", OWN, 3000, "Corrected body"),
        message(),
        reaction("$other-reaction", "@alex:example.test"),
        reaction("$own-reaction", OWN),
        reaction("$own-duplicate", OWN),
        edit("$forged", "@stranger:example.test", 5000, "Forged edit"),
    ];
    server
        .state
        .lock()
        .unwrap()
        .matrix_events
        .insert(ROOM.into(), events);
    let adapter = Registry::default()
        .connect(config(&server), Some("test-token".into()))
        .unwrap();
    let key = RoomKey::new(fixture::account_id(1), ROOM);
    for _ in 0..2 {
        let batch = adapter.sync(None).await.unwrap();
        let messages = &batch
            .conversations
            .iter()
            .find(|room| room.key == key)
            .unwrap()
            .messages;
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].body, "Corrected body");
        assert_eq!(messages[0].reactions.len(), 1);
        assert_eq!(messages[0].reactions[0].count, 2);
        assert!(messages[0].reactions[0].own);
        let history = adapter.history(&key, None).await.unwrap();
        assert_eq!(history[0].body, "Corrected body");
        assert_eq!(history[0].reactions[0].count, 2);
    }
    server
        .state
        .lock()
        .unwrap()
        .matrix_events
        .get_mut(ROOM)
        .unwrap()
        .extend([
            redact("$remove1", "$own-reaction"),
            redact("$remove2", "$own-duplicate"),
            redact("$remove-edit", "$edit"),
        ]);
    let history = adapter.history(&key, None).await.unwrap();
    assert_eq!(history[0].body, "Original body");
    assert!(!history[0].edited);
    assert_eq!(history[0].reactions[0].count, 1);
    assert!(!history[0].reactions[0].own);
    server
        .state
        .lock()
        .unwrap()
        .matrix_events
        .get_mut(ROOM)
        .unwrap()
        .extend([redact("$delete", "$base"), message()]);
    let history = adapter.history(&key, None).await.unwrap();
    assert!(history[0].deleted);
    assert!(history[0].body.is_empty());
    assert!(history[0].reactions.is_empty());
    // A fresh connection sees deletion before the original while walking backward.
    let fresh = Registry::default()
        .connect(config(&server), Some("test-token".into()))
        .unwrap();
    let history = fresh.history(&key, None).await.unwrap();
    assert!(history[0].deleted);
    assert!(history[0].body.is_empty());
}

#[tokio::test]
async fn matrix_events_rest_actions_survive_sync_and_remove_the_actual_reaction_event() {
    use messages_protocols::MessageAction;
    let server = TestServer::start().await.unwrap();
    let adapter = Registry::default()
        .connect(config(&server), Some("test-token".into()))
        .unwrap();
    let batch = adapter.sync(None).await.unwrap();
    let room = batch
        .conversations
        .iter()
        .find(|room| room.messages.iter().any(|message| message.own))
        .unwrap();
    let message = room
        .messages
        .iter()
        .find(|message| message.own)
        .unwrap()
        .clone();
    let key = &room.key;
    adapter
        .action(
            key,
            MessageAction::Edit {
                message: message.clone(),
                body: "Corrected through REST".into(),
            },
        )
        .await
        .unwrap();
    adapter
        .action(
            key,
            MessageAction::React {
                message: message.clone(),
                emoji: "👍".into(),
                remove: false,
            },
        )
        .await
        .unwrap();
    let batch = adapter.sync(None).await.unwrap();
    let current = batch
        .conversations
        .iter()
        .find(|room| &room.key == key)
        .unwrap()
        .messages
        .iter()
        .find(|m| m.id == message.id)
        .unwrap();
    assert_eq!(current.body, "Corrected through REST");
    assert!(current.edited);
    assert_eq!(current.reactions[0].count, 1);
    adapter
        .action(
            key,
            MessageAction::React {
                message: current.clone(),
                emoji: "👍".into(),
                remove: true,
            },
        )
        .await
        .unwrap();
    let history = adapter.history(key, None).await.unwrap();
    let current = history.iter().find(|m| m.id == message.id).unwrap();
    assert_eq!(current.body, "Corrected through REST");
    assert!(current.reactions.is_empty());
    let state = server.state.lock().unwrap();
    let removed = state.matrix_events[&key.room]
        .iter()
        .find(|event| event["type"] == "m.room.redaction")
        .unwrap();
    let reaction = state.matrix_events[&key.room]
        .iter()
        .find(|event| event["type"] == "m.reaction")
        .unwrap();
    assert_eq!(removed["content"]["redacts"], reaction["event_id"]);
}
