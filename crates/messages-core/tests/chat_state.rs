use messages_core::{DeviceRead, MessageStore, RoomKey, fixture};

#[test]
fn chat_state_partial_and_early_reads_survive_replay_without_clearing_newer_unread() {
    {
        let mut store = fixture::sample_store();
        let key = store.conversations[0].key.clone();
        let mut inbound: Vec<_> = store
            .room(&key)
            .unwrap()
            .messages
            .iter()
            .filter(|message| !message.own)
            .cloned()
            .collect();
        let read = inbound.pop().unwrap();
        let old = inbound.first().unwrap().clone();
        store.room_mut(&key).unwrap().unread = 1;
        let update = DeviceRead {
            account: key.account,
            sender: read.sender.clone(),
            timestamp: read.timestamp.timestamp_millis(),
        };
        store.apply_device_read(DeviceRead {
            account: key.account,
            sender: old.sender,
            timestamp: old.timestamp.timestamp_millis(),
        });
        assert_eq!(store.room(&key).unwrap().unread, 1);
        store.apply_device_read(update.clone());
        assert_eq!(store.room(&key).unwrap().unread, 0);
        store.apply_device_read(update);
        assert_eq!(store.room(&key).unwrap().unread, 0);
        let mut restored: MessageStore =
            serde_json::from_slice(&serde_json::to_vec(&store).unwrap()).unwrap();
        restored.merge_messages(&key, vec![read.clone()]).unwrap();
        assert!(restored.message_is_read(&key, &read));
        assert_eq!(restored.room(&key).unwrap().unread, 0);
        let mut unseen = read.clone();
        unseen.id = "future".into();
        unseen.timestamp += chrono::Duration::seconds(1);
        assert!(!restored.message_is_read(&key, &unseen));
        let other = RoomKey::new(fixture::account_id(1), &key.room);
        assert!(!restored.message_is_read(&other, &read));
    }

    {
        let mut store = fixture::sample_store();
        let key = store.conversations[0].key.clone();
        let mut messages: Vec<_> = store
            .room(&key)
            .unwrap()
            .messages
            .iter()
            .filter(|message| !message.own)
            .cloned()
            .collect();
        store.room_mut(&key).unwrap().messages.clear();
        let first = messages.remove(0);
        store.apply_device_read(DeviceRead {
            account: key.account,
            sender: first.sender.clone(),
            timestamp: first.timestamp.timestamp_millis(),
        });
        store.merge_messages(&key, vec![first.clone()]).unwrap();
        assert!(store.message_is_read(&key, &first));
        let second = messages.remove(0);
        let third = messages.remove(0);
        store
            .merge_messages(&key, vec![second.clone(), third.clone()])
            .unwrap();
        store.room_mut(&key).unwrap().unread = 2;
        store.mark_chat_read(&key, second.timestamp.timestamp_millis());
        assert_eq!(store.room(&key).unwrap().unread, 1);
        assert!(store.message_is_read(&key, &second));
        assert!(!store.message_is_read(&key, &third));
        store.mark_chat_read(&key, first.timestamp.timestamp_millis());
        assert_eq!(store.room(&key).unwrap().unread, 1);
    }
}
