use super::Snapshot;
use messages_core::MessageContentState;
use serde::Deserialize;
use std::{collections::BTreeMap, io::Read, path::Path};

#[derive(Default, Deserialize)]
struct CacheIndex {
    #[serde(default)]
    matrix: TimelineIndex,
}
#[derive(Default, Deserialize)]
struct TimelineIndex {
    #[serde(default)]
    rooms: BTreeMap<String, RoomIndex>,
}
#[derive(Deserialize)]
struct RoomIndex {
    messages: BTreeMap<String, MessageIndex>,
    #[serde(default)]
    reactions: BTreeMap<String, serde::de::IgnoredAny>,
    #[serde(default)]
    edits: BTreeMap<String, serde::de::IgnoredAny>,
}
#[derive(Deserialize)]
struct MessageIndex {
    encrypted: bool,
}

pub(super) fn restore_legacy_flags(snapshot: &mut Snapshot, directory: &Path) {
    for config in &snapshot.connections {
        if config.protocol.as_str() != "matrix"
            || !snapshot.store.accounts.iter().any(|account| {
                account.id == config.account && account.protocol.as_str() == "matrix"
            })
        {
            continue;
        }
        if !matches!(
            config.managed,
            Some(messages_protocols::ManagedConnection::Matrix { .. })
        ) {
            continue;
        }
        if !snapshot.store.conversations.iter().any(|room| {
            room.key.account == config.account
                && room
                    .messages
                    .iter()
                    .any(|message| message.content_state.is_none())
        }) {
            continue;
        }
        let path = directory
            .join("services")
            .join(config.account.to_string())
            .join("messages.json");
        let index = enzui_iced_core::atomic_state::open_state_file_nofollow(&path)
            .ok()
            .and_then(|file| {
                serde_json::from_reader::<_, CacheIndex>(file.take(32 * 1024 * 1024)).ok()
            })
            .unwrap_or_default();
        // Legacy snapshots have no typed content state; only the account's SDK index can identify ciphertext.
        for room in snapshot
            .store
            .conversations
            .iter_mut()
            .filter(|room| room.key.account == config.account)
        {
            for message in room
                .messages
                .iter_mut()
                .filter(|message| message.content_state.is_none())
            {
                if index.matrix.rooms.get(&room.key.room).is_some_and(|room| {
                    room.reactions.contains_key(&message.id) || room.edits.contains_key(&message.id)
                }) {
                    message.content_state = Some(MessageContentState::Relation);
                    message.deleted = true;
                    message.body.clear();
                    message.attachments.clear();
                    message.reactions.clear();
                    message.reply_to = None;
                    continue;
                }
                let encrypted = !message.deleted
                    && index
                        .matrix
                        .rooms
                        .get(&room.key.room)
                        .and_then(|room| room.messages.get(&message.id))
                        .is_some_and(|entry| entry.encrypted);
                message.content_state = Some(if encrypted {
                    MessageContentState::WaitingForKeys
                } else {
                    MessageContentState::Available
                });
                if encrypted {
                    message.body.clear();
                }
            }
        }
    }
}
