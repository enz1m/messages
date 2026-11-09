use chrono::Utc;
use messages_core::{Capability, Delivery, SendError, StoreError, fixture};

#[test]
fn outbox_send_ack_and_echo_converge_to_one_message() {
    let mut store = fixture::sample_store();
    let key = store.conversations[0].key.clone();
    let before = store.room(&key).unwrap().messages.len();
    let request = store
        .queue_message(&key, "Hello from the real reducer".into(), Utc::now())
        .unwrap();
    store.begin_send(request.transaction).unwrap();
    let mut echo = store.room(&key).unwrap().messages.last().unwrap().clone();
    echo.transaction = None;
    echo.id = "server-42".into();
    echo.delivery = Delivery::Read;
    store.merge_messages(&key, vec![echo.clone()]).unwrap();
    store.acknowledge(&key, request.transaction, "server-42".into());
    store.merge_messages(&key, vec![echo]).unwrap();
    assert_eq!(store.room(&key).unwrap().messages.len(), before + 1);
    assert!(store.outbox.is_empty());
    assert_eq!(
        store
            .room(&key)
            .unwrap()
            .messages
            .iter()
            .find(|m| m.id == "server-42")
            .unwrap()
            .delivery,
        Delivery::Read
    );

    for extra in [0, 32] {
        let mut original = store.room(&key).unwrap().messages[0].clone();
        let first_transaction = uuid::Uuid::new_v4();
        let second_transaction = uuid::Uuid::new_v4();
        original.id = "first".into();
        original.transaction = Some(first_transaction);
        original.deleted = false;
        original.delivery = Delivery::Read;
        let mut second = original.clone();
        second.id = "second".into();
        second.transaction = Some(second_transaction);
        store.room_mut(&key).unwrap().messages = vec![original.clone(), second.clone()];
        let mut conflicting_alias = second.clone();
        conflicting_alias.transaction = Some(first_transaction);
        conflicting_alias.body = "Earliest alias wins".into();
        let mut renamed = conflicting_alias.clone();
        renamed.id = "renamed".into();
        let mut duplicate_id = second.clone();
        duplicate_id.body = "Remaining duplicate ID".into();
        let mut old_id = original.clone();
        old_id.transaction = None;
        old_id.body = "Old ID no longer aliases".into();
        let mut incoming = vec![conflicting_alias, renamed, duplicate_id, old_id.clone()];
        let fillers: Vec<_> = (0..extra)
            .map(|i| {
                let mut message = old_id.clone();
                message.id = format!("filler-{i}");
                message.timestamp += chrono::Duration::seconds(1);
                message
            })
            .collect();
        incoming.extend(fillers.iter().cloned());
        store.merge_messages(&key, incoming).unwrap();
        let messages = &store.room(&key).unwrap().messages;
        assert_eq!(messages.len(), 3 + extra);
        assert_eq!(messages[0].id, "renamed");
        assert_eq!(messages[0].transaction, Some(first_transaction));
        assert_eq!(messages[1].body, "Remaining duplicate ID");
        assert_eq!(messages[2].id, "first");
        assert_eq!(messages[2].transaction, None);
        let mut replaced_transaction = messages[0].clone();
        replaced_transaction.transaction = Some(second_transaction);
        replaced_transaction.body = "Reassigned transaction".into();
        let mut incoming = vec![replaced_transaction, original];
        incoming.extend(fillers);
        store.merge_messages(&key, incoming).unwrap();
        let messages = &store.room(&key).unwrap().messages;
        assert_eq!(messages.len(), 3 + extra);
        assert_eq!(messages[0].body, "Reassigned transaction");
        assert_eq!(messages[2].transaction, Some(first_transaction));
    }
}

#[test]
fn outbox_failure_retains_text_and_manual_retry_identity() {
    let mut store = fixture::sample_store();
    let key = store.conversations[0].key.clone();
    let request = store
        .queue_message(&key, "Keep this text".into(), Utc::now())
        .unwrap();
    store.begin_send(request.transaction).unwrap();
    store.fail_send(
        &key,
        request.transaction,
        SendError::Rejected("Server unavailable".into()),
    );
    let retry = store.begin_send(request.transaction).unwrap();
    assert_eq!(request.transaction, retry.transaction);
    assert_eq!(retry.body, "Keep this text");
    assert_eq!(store.outbox[0].attempts, 2);
    assert!(matches!(
        store.begin_send(request.transaction),
        Err(StoreError::AlreadySending)
    ));
}

#[test]
fn capabilities_account_and_room_refusals_preserve_the_draft() {
    {
        let mut store = fixture::sample_store();
        let key = store.conversations[0].key.clone();
        let room = store.room_mut(&key).unwrap();
        room.draft = "My draft".into();
        room.capabilities
            .disable(Capability::SendText, "Only moderators can post here.");
        let before = room.messages.len();
        assert!(
            matches!(store.queue_message(&key, "My draft".into(), Utc::now()),
        Err(StoreError::Unsupported(reason)) if reason == "Only moderators can post here.")
        );
        assert_eq!(store.room(&key).unwrap().draft, "My draft");
        assert_eq!(store.room(&key).unwrap().messages.len(), before);
        assert!(store.outbox.is_empty());
    }

    {
        let mut store = fixture::sample_store();
        let key = store.conversations[0].key.clone();
        let reply = store.room(&key).unwrap().messages[0].id.clone();
        store.accounts[0]
            .capabilities
            .disable(Capability::Reply, "This account cannot quote messages.");
        let room = store.room_mut(&key).unwrap();
        room.reply_to = Some(reply.clone());
        room.draft = "Keep my reply draft".into();
        let before = room.messages.len();
        assert!(matches!(
            store.queue_message(&key, "Keep my reply draft".into(), Utc::now()),
            Err(StoreError::Unsupported(_))
        ));
        let room = store.room(&key).unwrap();
        assert_eq!(room.reply_to.as_ref(), Some(&reply));
        assert_eq!(room.draft, "Keep my reply draft");
        assert_eq!(room.messages.len(), before);
        assert!(store.outbox.is_empty());
        store.room_mut(&key).unwrap().reply_to = None;
        assert!(
            store
                .queue_message(&key, "Plain text remains supported".into(), Utc::now())
                .is_ok()
        );
        assert_eq!(store.outbox.len(), 1);
    }
}

#[test]
fn account_scoping_prevents_cross_account_message_mutation() {
    let mut store = fixture::sample_store();
    let key = store.conversations[0].key.clone();
    let mut other = store.conversations[0].clone();
    other.key.account = store.accounts[1].id;
    let other_key = other.key.clone();
    store.conversations.push(other);
    let request = store
        .queue_message(&key, "Only the first account".into(), Utc::now())
        .unwrap();
    store.acknowledge(&other_key, request.transaction, "wrong-account".into());
    assert_eq!(store.outbox.len(), 1);
    assert_eq!(
        store.room(&key).unwrap().messages.last().unwrap().delivery,
        Delivery::Queued
    );
    assert!(
        !store
            .room(&other_key)
            .unwrap()
            .messages
            .iter()
            .any(|m| m.body == request.body)
    );
}
