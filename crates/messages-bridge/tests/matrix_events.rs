use messages_bridge::cache::Cache;
use messages_core::{Capabilities, Conversation, Encryption, RoomKey, RoomKind};
use serde_json::json;

#[tokio::test]
async fn matrix_events_cache_keeps_reaction_ids_and_tombstones_across_restart() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cache.json");
    let key = RoomKey::new(uuid::Uuid::nil(), "!room:example.test");
    let cache = Cache::open(path.clone()).await.unwrap();
    let own = "@you:example.test";
    let message = json!({"event_id":"$base","type":"m.room.message","sender":own,"origin_server_ts":1000,"content":{"body":"Private original"}});
    let reaction = json!({"event_id":"$reaction","type":"m.reaction","sender":own,"content":{"m.relates_to":{"rel_type":"m.annotation","event_id":"$base","key":"👍"}}});
    {
        let mut cache = cache.lock().await;
        cache.store.conversations.push(Conversation::empty(
            key.clone(),
            "Room",
            RoomKind::Direct,
            Encryption::EndToEnd,
            Capabilities::default(),
        ));
        cache.apply_matrix(&key, &message, own).unwrap();
        let encrypted_reaction = json!({"event_id":"$reaction","type":"m.room.encrypted","sender":own,"origin_server_ts":1001,"content":{"algorithm":"m.megolm.v1.aes-sha2","ciphertext":"synthetic-not-real"}});
        cache.apply_matrix(&key, &encrypted_reaction, own).unwrap();
        assert_eq!(cache.store.room(&key).unwrap().messages.len(), 2);
        cache.apply_matrix(&key, &reaction, own).unwrap();
        assert_eq!(
            cache.store.room(&key).unwrap().readable_messages().count(),
            1
        );
        assert!(!cache.store.room(&key).unwrap().has_locked_history());
        cache.apply_matrix(&key, &encrypted_reaction, own).unwrap();
        assert_eq!(
            cache.store.room(&key).unwrap().readable_messages().count(),
            1
        );
        assert!(!cache.store.room(&key).unwrap().has_locked_history());
        cache.persist().await.unwrap();
    }
    drop(cache);
    let restored = Cache::open(path.clone()).await.unwrap();
    {
        let mut cache = restored.lock().await;
        assert_eq!(
            cache.matrix.own_reaction_ids(&key.room, "$base", "👍", own),
            ["$reaction"]
        );
        cache.apply_matrix(&key, &reaction, own).unwrap();
        assert_eq!(
            cache.store.room(&key).unwrap().messages[0].reactions[0].count,
            1
        );
        cache.apply_matrix(&key, &json!({"event_id":"$redact","type":"m.room.redaction","content":{"redacts":"$base"}}), own).unwrap();
        cache.persist().await.unwrap();
    }
    assert!(
        !std::fs::read_to_string(&path)
            .unwrap()
            .contains("Private original")
    );
    drop(restored);
    let restored = Cache::open(path).await.unwrap();
    let mut cache = restored.lock().await;
    cache.apply_matrix(&key, &message, own).unwrap();
    assert!(cache.store.room(&key).unwrap().messages[0].deleted);
    assert!(cache.store.room(&key).unwrap().messages[0].body.is_empty());
}
