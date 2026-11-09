use chrono::Utc;
use messages_core::{
    Capability, ChangeKind, Delivery, MessageChange, MessageStore, OutgoingAttachment, StoreError,
    fixture,
};
use uuid::Uuid;

#[test]
fn media_outbox_attachment_only_send_is_durable_and_keeps_retry_identity() {
    let mut store = fixture::sample_store();
    let key = store.conversations[0].key.clone();
    store.accounts[0].capabilities = messages_core::Capabilities::new(Capability::ALL);
    store.room_mut(&key).unwrap().capabilities = messages_core::Capabilities::new(Capability::ALL);
    let file = OutgoingAttachment {
        id: Uuid::new_v4(),
        path: "/private/report.pdf".into(),
        name: "report.pdf".into(),
        mime: "application/pdf".into(),
        bytes: 45,
    };
    store
        .room_mut(&key)
        .unwrap()
        .draft_attachments
        .push(file.clone());
    let request = store
        .queue_with_attachments(&key, String::new(), vec![file], Utc::now())
        .unwrap();
    assert_eq!(request.attachments.len(), 1);
    assert!(store.room(&key).unwrap().draft_attachments.is_empty());
    let snapshot = serde_json::to_vec(&store).unwrap();
    let mut restored: MessageStore = serde_json::from_slice(&snapshot).unwrap();
    let retry = restored.begin_send(request.transaction).unwrap();
    assert_eq!(retry.attachments[0].id, request.attachments[0].id);
    assert_eq!(retry.attachments[0].path, request.attachments[0].path);
    assert!(retry.body.is_empty());
    assert_eq!(
        restored
            .room(&key)
            .unwrap()
            .messages
            .last()
            .unwrap()
            .attachments[0]
            .name,
        "report.pdf"
    );
}

#[test]
fn media_outbox_rejects_unsupported_files_without_mutating_the_draft() {
    let mut store = fixture::sample_store();
    let key = store.conversations[0].key.clone();
    store.accounts[0]
        .capabilities
        .disable(Capability::AttachFiles, "Unsupported");
    store.room_mut(&key).unwrap().draft = "Keep me".into();
    let file = OutgoingAttachment {
        id: Uuid::new_v4(),
        path: "/private/file.txt".into(),
        name: "file.txt".into(),
        mime: "text/plain".into(),
        bytes: 1,
    };
    assert!(matches!(
        store.queue_with_attachments(&key, "Keep me".into(), vec![file], Utc::now()),
        Err(StoreError::Unsupported(_))
    ));
    assert!(store.outbox.is_empty());
    assert_eq!(store.room(&key).unwrap().draft, "Keep me");
}

#[test]
fn media_revisions_and_receipts_survive_reordering_duplicates_and_restart() {
    {
        let mut store = fixture::sample_store();
        let key = store.conversations[0].key.clone();
        let original = store.room(&key).unwrap().messages[0].clone();
        let change = |kind| MessageChange {
            account: key.account,
            room: Some(key.room.clone()),
            message: original.id.clone(),
            kind,
        };
        store.apply_change(change(ChangeKind::Edit {
            timestamp: 20,
            body: "New text".into(),
        }));
        store.apply_change(change(ChangeKind::Edit {
            timestamp: 10,
            body: "Old edit".into(),
        }));
        let reaction = change(ChangeKind::Reaction {
            timestamp: 21,
            actor: "Maya".into(),
            emoji: "👍".into(),
            remove: false,
            own: false,
        });
        store.apply_change(reaction.clone());
        store.apply_change(reaction);
        store.merge_messages(&key, vec![original.clone()]).unwrap();
        let target = &store.room(&key).unwrap().messages[0];
        assert_eq!(target.body, "New text");
        assert_eq!(target.reactions[0].count, 1);
        let mut restored: MessageStore =
            serde_json::from_slice(&serde_json::to_vec(&store).unwrap()).unwrap();
        restored.apply_change(change(ChangeKind::Delete));
        restored.merge_messages(&key, vec![original]).unwrap();
        let target = &restored.room(&key).unwrap().messages[0];
        assert!(target.deleted);
        assert!(target.body.is_empty());
        assert!(target.reactions.is_empty());
    }

    {
        let mut store = fixture::sample_store();
        let key = store.conversations[0].key.clone();
        let mut unseen = store.room(&key).unwrap().messages[0].clone();
        unseen.id = "unseen".into();
        store.apply_change(MessageChange {
            account: key.account,
            room: Some(key.room.clone()),
            message: unseen.id.clone(),
            kind: ChangeKind::Delete,
        });
        store.merge_messages(&key, vec![unseen]).unwrap();
        assert!(
            store
                .room(&key)
                .unwrap()
                .messages
                .iter()
                .find(|message| message.id == "unseen")
                .unwrap()
                .deleted
        );
        let own = store
            .room(&key)
            .unwrap()
            .messages
            .iter()
            .find(|message| message.own)
            .unwrap()
            .clone();
        for delivery in [Delivery::Read, Delivery::Delivered] {
            store.apply_change(MessageChange {
                account: key.account,
                room: None,
                message: own.id.clone(),
                kind: ChangeKind::Receipt(delivery),
            });
        }
        store.merge_messages(&key, vec![own.clone()]).unwrap();
        assert_eq!(
            store
                .room(&key)
                .unwrap()
                .messages
                .iter()
                .find(|message| message.id == own.id)
                .unwrap()
                .delivery,
            Delivery::Read
        );
    }
}
