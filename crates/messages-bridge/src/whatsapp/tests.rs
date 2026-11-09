use std::{io::Write, sync::Arc};

use chrono::{TimeZone, Utc};
use messages_core::{Delivery, RoomKey};
use whatsapp_rust::{
    prelude::*,
    wacore::types::{
        events::{BatchOrigin, InboundMessage, LazyHistorySync, MessageBatch, Receipt},
        message::{MessageInfo, MessageSource},
        presence::ReceiptType,
    },
};

use super::events;
use crate::cache::{Cache, SharedCache};

const OWN: &str = "15550100001@s.whatsapp.net";
const ALEX: &str = "15550100002@s.whatsapp.net";
const BOB: &str = "15550100003@s.whatsapp.net";
const GROUP: &str = "120363000000001@g.us";

fn own() -> Vec<String> {
    vec![OWN.into()]
}
fn key(chat: &str) -> RoomKey {
    RoomKey::new(uuid::Uuid::nil(), chat)
}

fn info(chat: &str, sender: &str, id: &str) -> MessageInfo {
    MessageInfo {
        source: MessageSource {
            chat: chat.parse().unwrap(),
            sender: sender.parse().unwrap(),
            is_from_me: sender == OWN,
            is_group: chat.ends_with("@g.us"),
            ..Default::default()
        },
        id: id.parse().unwrap(),
        push_name: if sender == OWN {
            "You"
        } else if sender == ALEX {
            "Alex"
        } else {
            "Bob"
        }
        .into(),
        timestamp: Utc.timestamp_opt(1000, 0).unwrap(),
        ..Default::default()
    }
}

async fn incoming(
    cache: &SharedCache,
    info: MessageInfo,
    message: wa::Message,
    expiry: Option<u32>,
) {
    let inbound = InboundMessage::builder()
        .message(Arc::new(message))
        .info(Arc::new(info))
        .maybe_ephemeral_expiration(expiry)
        .build();
    let event = Event::Messages(
        MessageBatch::builder()
            .messages(Arc::from([inbound]))
            .origin(BatchOrigin::Live)
            .build(),
    );
    events::dispatch(cache, Arc::new(event), own())
        .await
        .unwrap();
}

fn reference(chat: &str, id: &str, from_me: bool, participant: Option<&str>) -> wa::MessageKey {
    wa::MessageKey {
        remote_jid: Some(chat.into()),
        id: Some(id.into()),
        from_me: Some(from_me),
        participant: participant.map(str::to_owned),
    }
}

fn reaction(target: wa::MessageKey, text: &str, time: i64) -> wa::Message {
    wa::Message {
        reaction_message: wa::message::ReactionMessage {
            key: target.into(),
            text: Some(text.into()),
            sender_timestamp_ms: Some(time),
            ..Default::default()
        }
        .into(),
        ..Default::default()
    }
}

fn protocol(target: wa::MessageKey, text: Option<&str>, time: i64) -> wa::Message {
    wa::Message {
        protocol_message: wa::message::ProtocolMessage {
            key: target.into(),
            r#type: Some(if text.is_some() {
                wa::message::protocol_message::Type::MESSAGE_EDIT
            } else {
                wa::message::protocol_message::Type::REVOKE
            }),
            edited_message: text
                .map(wa::Message::text)
                .map(Into::into)
                .unwrap_or_default(),
            timestamp_ms: Some(time),
            ..Default::default()
        }
        .into(),
        ..Default::default()
    }
}

fn history_message(chat: &str, sender: &str, id: &str, text: &str) -> wa::HistorySyncMsg {
    wa::HistorySyncMsg {
        message: wa::WebMessageInfo {
            key: reference(chat, id, sender == OWN, Some(sender)).into(),
            message: wa::Message::text(text).into(),
            message_timestamp: Some(1000),
            push_name: Some("Fixture person".into()),
            status: Some(wa::web_message_info::Status::DELIVERY_ACK),
            ..Default::default()
        }
        .into(),
        ..Default::default()
    }
}

fn history(conversations: Vec<wa::Conversation>) -> Arc<Event> {
    let bytes = whatsapp_rust::waproto::codec::history_sync_to_vec(&wa::HistorySync {
        conversations,
        ..Default::default()
    });
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&bytes).unwrap();
    Arc::new(Event::HistorySync(Box::new(LazyHistorySync::new(
        encoder.finish().unwrap().into(),
        bytes.len(),
        0,
        None,
        None,
    ))))
}

#[tokio::test]
async fn whatsapp_events_import_compressed_history_and_preserve_sender_scoped_actions() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("messages.json");
    let cache = Cache::open(path.clone()).await.unwrap();
    let original = history(vec![wa::Conversation {
        id: GROUP.into(),
        name: Some("Family".into()),
        unread_count: Some(2),
        messages: vec![
            history_message(GROUP, ALEX, "same-id", "Alex original"),
            history_message(GROUP, BOB, "same-id", "Bob original"),
        ],
        ..Default::default()
    }]);
    events::dispatch(&cache, original.clone(), own())
        .await
        .unwrap();
    assert_eq!(
        cache
            .lock()
            .await
            .store
            .room(&key(GROUP))
            .unwrap()
            .messages
            .len(),
        2
    );
    incoming(
        &cache,
        info(GROUP, ALEX, "edit"),
        protocol(
            reference(GROUP, "same-id", true, None),
            Some("Alex corrected"),
            2000,
        ),
        None,
    )
    .await;
    incoming(
        &cache,
        info(GROUP, BOB, "forged"),
        protocol(
            reference(GROUP, "same-id", false, Some(ALEX)),
            Some("Forged body"),
            3000,
        ),
        None,
    )
    .await;
    incoming(
        &cache,
        info(GROUP, BOB, "reaction"),
        reaction(reference(GROUP, "same-id", false, Some(ALEX)), "👍", 2000),
        None,
    )
    .await;
    events::dispatch(&cache, original.clone(), own())
        .await
        .unwrap();
    {
        let cache = cache.lock().await;
        let room = cache.store.room(&key(GROUP)).unwrap();
        assert_eq!(room.title, "Family");
        let alex = room
            .messages
            .iter()
            .find(|message| message.sender == ALEX)
            .unwrap();
        assert_eq!(alex.body, "Alex corrected");
        assert!(alex.edited);
        assert_eq!(alex.reactions[0].count, 1);
        assert_eq!(
            room.messages
                .iter()
                .find(|message| message.sender == BOB)
                .unwrap()
                .body,
            "Bob original"
        );
    }
    incoming(
        &cache,
        info(GROUP, ALEX, "revoke"),
        protocol(reference(GROUP, "same-id", true, None), None, 4000),
        None,
    )
    .await;
    let restored = Cache::open(path.clone()).await.unwrap();
    events::dispatch(&restored, original, own()).await.unwrap();
    let cache = restored.lock().await;
    let room = cache.store.room(&key(GROUP)).unwrap();
    let alex = room
        .messages
        .iter()
        .find(|message| message.sender == ALEX)
        .unwrap();
    assert!(alex.deleted);
    assert!(alex.body.is_empty());
    assert!(alex.reactions.is_empty());
    assert!(
        !room
            .messages
            .iter()
            .find(|message| message.sender == BOB)
            .unwrap()
            .deleted
    );
    let saved = std::fs::read_to_string(path).unwrap();
    assert!(!saved.contains("Alex original") && !saved.contains("Alex corrected"));
}

#[tokio::test]
async fn whatsapp_events_reactions_keep_latest_per_actor_and_receipts_survive_echo() {
    let directory = tempfile::tempdir().unwrap();
    let cache = Cache::open(directory.path().join("messages.json"))
        .await
        .unwrap();
    incoming(
        &cache,
        info(ALEX, OWN, "base"),
        wa::Message::text("Own message"),
        None,
    )
    .await;
    for _ in 0..2 {
        incoming(
            &cache,
            info(ALEX, ALEX, "react"),
            reaction(reference(OWN, "base", false, None), "👍", 2000),
            None,
        )
        .await;
    }
    incoming(
        &cache,
        info(ALEX, OWN, "own-react"),
        reaction(reference(ALEX, "base", true, None), "👍", 2000),
        None,
    )
    .await;
    assert_eq!(
        cache.lock().await.store.room(&key(ALEX)).unwrap().messages[0].reactions[0].count,
        2
    );
    incoming(
        &cache,
        info(ALEX, ALEX, "remove"),
        reaction(reference(OWN, "base", false, None), "", 3000),
        None,
    )
    .await;
    incoming(
        &cache,
        info(ALEX, ALEX, "stale"),
        reaction(reference(OWN, "base", false, None), "👍", 2000),
        None,
    )
    .await;
    let receipt = Receipt::builder()
        .source(info(ALEX, ALEX, "unused").source)
        .message_ids(vec!["base".parse().unwrap()])
        .timestamp(Utc.timestamp_opt(3000, 0).unwrap())
        .r#type(ReceiptType::Read)
        .offline(false)
        .build();
    events::dispatch(&cache, Arc::new(Event::Receipt(receipt)), own())
        .await
        .unwrap();
    incoming(
        &cache,
        info(ALEX, OWN, "base"),
        wa::Message::text("Own message"),
        None,
    )
    .await;
    let cache = cache.lock().await;
    let message = &cache.store.room(&key(ALEX)).unwrap().messages[0];
    assert_eq!(message.reactions[0].count, 1);
    assert!(message.reactions[0].own);
    assert_eq!(message.delivery, Delivery::Read);
}

#[tokio::test]
async fn whatsapp_events_reply_identity_and_disappearing_content_are_not_flattened_to_plaintext() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("messages.json");
    let cache = Cache::open(path.clone()).await.unwrap();
    incoming(
        &cache,
        info(ALEX, ALEX, "reply"),
        wa::Message::text_with_context(
            "A reply",
            wa::ContextInfo {
                stanza_id: Some("base".into()),
                participant: Some(OWN.into()),
                ..Default::default()
            },
        ),
        None,
    )
    .await;
    incoming(
        &cache,
        info(ALEX, ALEX, "ephemeral"),
        wa::Message::text("Do not retain this expiring plaintext"),
        Some(86400),
    )
    .await;
    incoming(
        &cache,
        info(ALEX, ALEX, "ephemeral-edit"),
        protocol(
            reference(OWN, "ephemeral", true, None),
            Some("Do not retain the edit either"),
            4000,
        ),
        None,
    )
    .await;
    incoming(
        &cache,
        info(ALEX, ALEX, "ephemeral"),
        wa::Message::text("Do not retain unflagged stale copies"),
        None,
    )
    .await;
    let cache = cache.lock().await;
    let room = cache.store.room(&key(ALEX)).unwrap();
    assert_eq!(room.messages[0].reply_to.as_deref(), Some("own|base"));
    assert!(room.messages[1].body.contains("Disappearing message"));
    assert!(
        !std::fs::read_to_string(path)
            .unwrap()
            .contains("Do not retain")
    );
}

#[tokio::test]
async fn whatsapp_events_linked_device_read_before_message_survives_restart() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cache.json");
    let cache = Cache::open(path.clone()).await.unwrap();
    let receipt = Receipt::builder()
        .source(info(ALEX, OWN, "read-first").source)
        .message_ids(vec!["read-first".parse().unwrap()])
        .timestamp(Utc.timestamp_opt(1001, 0).unwrap())
        .r#type(ReceiptType::ReadSelf)
        .offline(false)
        .build();
    events::dispatch(&cache, Arc::new(Event::Receipt(receipt)), own())
        .await
        .unwrap();
    drop(cache);
    let cache = Cache::open(path).await.unwrap();
    incoming(
        &cache,
        info(ALEX, ALEX, "read-first"),
        wa::Message {
            conversation: Some("Read on phone".into()),
            ..Default::default()
        },
        None,
    )
    .await;
    let mut newer = info(ALEX, ALEX, "unread-newer");
    newer.timestamp = Utc.timestamp_opt(1030, 0).unwrap();
    incoming(
        &cache,
        newer,
        wa::Message {
            conversation: Some("Still unread".into()),
            ..Default::default()
        },
        None,
    )
    .await;
    let cache = cache.lock().await;
    let room = cache.store.room(&key(ALEX)).unwrap();
    assert!(cache.store.message_is_read(&key(ALEX), &room.messages[0]));
    assert!(!cache.store.message_is_read(&key(ALEX), &room.messages[1]));
}

#[tokio::test]
async fn contact_names_repair_existing_ids_respect_saved_names_and_survive_outbound_and_history_replay()
 {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("profiles.json");
    let cache = Cache::open(path.clone()).await.unwrap();
    incoming(
        &cache,
        info(ALEX, ALEX, "name1"),
        wa::Message::text("Hello"),
        None,
    )
    .await;
    assert_eq!(
        cache.lock().await.store.room(&key(ALEX)).unwrap().title,
        "Alex"
    );
    let action = wa::sync_action_value::ContactAction {
        full_name: Some("Alex Contact".into()),
        lid_jid: Some("123456@lid".into()),
        ..Default::default()
    };
    let event = Event::ContactUpdate(
        whatsapp_rust::wacore::types::events::ContactUpdate::builder()
            .jid(ALEX.parse().unwrap())
            .timestamp(Utc.timestamp_opt(2000, 0).unwrap())
            .action(Box::new(action))
            .from_full_sync(true)
            .build(),
    );
    events::dispatch(&cache, Arc::new(event), own())
        .await
        .unwrap();
    incoming(
        &cache,
        info(ALEX, OWN, "name2"),
        wa::Message::text("Own reply"),
        None,
    )
    .await;
    let old = history(vec![wa::Conversation {
        id: ALEX.into(),
        name: Some("Old history name".into()),
        ..Default::default()
    }]);
    events::dispatch(&cache, old, own()).await.unwrap();
    assert_eq!(
        cache.lock().await.store.room(&key(ALEX)).unwrap().title,
        "Alex Contact"
    );
    let restored = Cache::open(path.clone()).await.unwrap();
    assert_eq!(
        restored.lock().await.store.room(&key(ALEX)).unwrap().title,
        "Alex Contact"
    );
    let mut restored = restored.lock().await;
    restored
        .store
        .conversations
        .push(messages_core::Conversation::empty(
            key("123456@lid"),
            "123456@lid",
            messages_core::RoomKind::Direct,
            messages_core::Encryption::EndToEnd,
            super::capabilities(),
        ));
    super::profiles::repair(&mut restored);
    assert_eq!(
        restored.store.room(&key("123456@lid")).unwrap().title,
        "Alex Contact"
    );
    assert_eq!(
        super::profiles::fallback("123456@lid"),
        "WhatsApp ID 123456"
    );
    assert_eq!(super::profiles::fallback(ALEX), "+15550100002");
    let removed = Event::ContactRemoved(
        whatsapp_rust::wacore::types::events::ContactRemoved::builder()
            .jid(ALEX.parse().unwrap())
            .timestamp(Utc.timestamp_opt(3000, 0).unwrap())
            .from_full_sync(false)
            .build(),
    );
    super::profiles::event(&mut restored, &removed);
    super::profiles::repair(&mut restored);
    assert_eq!(restored.store.room(&key(ALEX)).unwrap().title, "Alex");
    assert_ne!(
        restored.store.room(&key("123456@lid")).unwrap().title,
        "Alex Contact"
    );
    let unknown = "789012@lid";
    restored
        .store
        .conversations
        .push(messages_core::Conversation::empty(
            key(unknown),
            "WhatsApp contact",
            messages_core::RoomKind::Direct,
            messages_core::Encryption::EndToEnd,
            super::capabilities(),
        ));
    super::profiles::repair(&mut restored);
    assert_eq!(
        restored.store.room(&key(unknown)).unwrap().title,
        "WhatsApp ID 789012"
    );
    super::profiles::phone_alias(&mut restored, unknown, "15550107777");
    assert_eq!(
        restored.store.room(&key(unknown)).unwrap().title,
        "+15550107777"
    );
    super::profiles::phone_alias(&mut restored, unknown, "not a phone number");
    restored.persist().await.unwrap();
    drop(restored);
    let restored = Cache::open(path).await.unwrap();
    let mut restored = restored.lock().await;
    super::profiles::repair(&mut restored);
    assert_eq!(
        restored.store.room(&key(unknown)).unwrap().title,
        "+15550107777"
    );
    super::profiles::push_name(
        &mut restored,
        "15550107777@s.whatsapp.net",
        "Phone alias name",
    );
    super::profiles::repair(&mut restored);
    assert_eq!(
        restored.store.room(&key(unknown)).unwrap().title,
        "Phone alias name"
    );
    let sdk_directory = directory.path().join("sdk-mapping");
    std::fs::create_dir(&sdk_directory).unwrap();
    let (backend, _bot) = super::WhatsApp::prepare(&sdk_directory).await.unwrap();
    backend
        .cache
        .lock()
        .await
        .store
        .conversations
        .push(messages_core::Conversation::empty(
            key("654321@lid"),
            "WhatsApp contact",
            messages_core::RoomKind::Direct,
            messages_core::Encryption::EndToEnd,
            super::capabilities(),
        ));
    backend
        .client
        .add_lid_pn_mapping(
            "654321",
            "15550108888",
            whatsapp_rust::lid_pn_cache::LearningSource::Other,
        )
        .await
        .unwrap();
    backend.refresh_phone_numbers().await;
    assert_eq!(
        backend
            .cache
            .lock()
            .await
            .store
            .room(&key("654321@lid"))
            .unwrap()
            .title,
        "+15550108888"
    );
}
