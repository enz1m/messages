use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use messages_core::{ChatMessage, Delivery, Encryption, Reaction, RoomKey, RoomKind};
use serde::{Deserialize, Serialize};
use whatsapp_rust::{
    prelude::*,
    wacore::types::{
        message::{EditAttribute, MessageInfo},
        presence::ReceiptType,
    },
};

use crate::cache::{Cache, SharedCache};

#[derive(Default, Serialize, Deserialize)]
pub(crate) struct Timeline {
    #[serde(default)]
    pub(super) names: BTreeMap<String, super::profiles::Name>,
    #[serde(skip)]
    pub(super) contacts_imported: bool,
    #[serde(skip)]
    pub(super) names_dirty: bool,
    rooms: BTreeMap<String, BTreeMap<String, EventState>>,
    #[serde(default)]
    read_self_ids: BTreeMap<String, BTreeSet<String>>,
}

#[derive(Default, Serialize, Deserialize)]
struct EventState {
    deleted: bool,
    #[serde(default)]
    restricted: Option<String>,
    edit: Option<(i64, String)>,
    reactions: BTreeMap<String, Annotation>,
    receipt: u8,
    #[serde(default)]
    read_self: bool,
}

#[derive(Serialize, Deserialize)]
struct Annotation {
    time: i64,
    text: String,
    own: bool,
}

pub(super) async fn dispatch(
    cache: &SharedCache,
    event: Arc<Event>,
    own: Vec<String>,
) -> anyhow::Result<()> {
    if matches!(event.as_ref(), Event::HistorySync(_)) {
        let target = cache.clone();
        tokio::task::spawn_blocking(move || {
            let Event::HistorySync(history) = event.as_ref() else {
                return Ok(());
            };
            super::history::import(&mut target.blocking_lock(), history, &own)
        })
        .await??;
    } else {
        let mut cache = cache.lock().await;
        if super::profiles::event(&mut cache, event.as_ref()) {
            return Ok(());
        }
        match event.as_ref() {
            Event::ChatPresence(update) => {
                cache.set_typing(
                    update.source.chat.to_string(),
                    update.source.sender.to_string(),
                    matches!(
                        update.state,
                        whatsapp_rust::wacore::types::presence::ChatActivity::Typing
                            | whatsapp_rust::wacore::types::presence::ChatActivity::RecordingAudio
                    ),
                );
                return Ok(());
            }
            Event::Messages(batch) => {
                for inbound in batch.iter() {
                    receive(
                        &mut cache,
                        &inbound.message,
                        &inbound.info,
                        &own,
                        inbound.ephemeral_expiration,
                        false,
                    )?;
                }
            }
            Event::Receipt(receipt) => {
                // Linked-device reads clear incoming unread state; group delivery receipts
                // require member accounting before they can advance sent-message ticks.
                if matches!(receipt.r#type, ReceiptType::ReadSelf) {
                    cache
                        .whatsapp
                        .read_self_ids
                        .entry(receipt.source.chat.to_string())
                        .or_default()
                        .extend(receipt.message_ids.iter().map(|id| id.as_str().to_owned()));
                    let key = RoomKey::new(uuid::Uuid::nil(), receipt.source.chat.to_string());
                    let through = cache
                        .store
                        .room(&key)
                        .into_iter()
                        .flat_map(|room| &room.messages)
                        .filter(|m| {
                            !m.own
                                && receipt.message_ids.iter().any(|id| {
                                    super::remote_id(&m.id)
                                        .is_ok_and(|remote| remote.as_str() == id.as_str())
                                })
                        })
                        .map(|m| m.timestamp.timestamp_millis())
                        .max();
                    if let Some(through) = through {
                        cache.store.mark_chat_read(&key, through);
                    }
                    return cache.persist().await;
                }
                if receipt.source.is_from_me || receipt.source.is_group {
                    return Ok(());
                }
                let rank = match receipt.r#type {
                    ReceiptType::Delivered => 2,
                    ReceiptType::Read | ReceiptType::Played => 3,
                    _ => return Ok(()),
                };
                let chat = receipt.source.chat.to_string();
                for id in &receipt.message_ids {
                    let id = super::scoped_id(true, "", id.as_str());
                    let state = cache
                        .whatsapp
                        .rooms
                        .entry(chat.clone())
                        .or_default()
                        .entry(id.clone())
                        .or_default();
                    state.receipt = state.receipt.max(rank);
                    update(&mut cache, &chat, &id)?;
                }
            }
            _ => return Ok(()),
        }
    }
    cache.lock().await.persist().await
}

fn own_sender(sender: &str, own: &[String]) -> bool {
    own.iter().any(|id| id == sender)
}

fn local_target(
    key: &wa::MessageKey,
    info: &MessageInfo,
    own: &[String],
    history: bool,
) -> Option<String> {
    let remote = key.id.as_deref()?;
    let sender = info.source.sender.to_string();
    let (from_me, author) = if history || info.source.is_from_me {
        (
            key.from_me.unwrap_or(false),
            key.participant.as_deref().unwrap_or(&sender),
        )
    } else if key.from_me == Some(true) {
        (false, sender.as_str())
    } else if let Some(participant) = &key.participant {
        (own_sender(participant, own), participant.as_str())
    } else {
        (true, "")
    };
    Some(super::scoped_id(from_me, author, remote))
}

pub(super) fn receive(
    cache: &mut Cache,
    raw: &wa::Message,
    info: &MessageInfo,
    own: &[String],
    expiry: Option<u32>,
    history: bool,
) -> anyhow::Result<()> {
    let source = &info.source;
    let chat = source.chat.to_string();
    let sender = source.sender.to_string();
    let actor = if source.is_from_me { "own" } else { &sender };
    let base = raw.get_base_message();
    if let Some(reaction) = base.reaction_message.as_option() {
        if let Some(key) = reaction.key.as_option()
            && let Some(target) = local_target(key, info, own, history)
            && let Some(time) = reaction.sender_timestamp_ms
        {
            annotate(
                cache,
                &chat,
                &target,
                actor,
                reaction.text.as_deref().unwrap_or(""),
                time,
                source.is_from_me,
            )?;
        }
        return Ok(());
    }
    if let Some(protocol) = base.protocol_message.as_option() {
        if let Some(key) = protocol.key.as_option()
            && let Some(target) = local_target(key, info, own, history)
        {
            let authored = target
                .split_once('|')
                .is_some_and(|(author, _)| author == actor);
            let same_chat = history
                || !source.is_from_me
                || key
                    .remote_jid
                    .as_deref()
                    .is_none_or(|remote| remote == chat);
            let admin_revoke = source.is_group && info.edit == EditAttribute::AdminRevoke;
            if same_chat && (authored || admin_revoke) {
                if protocol.r#type == Some(wa::message::protocol_message::Type::REVOKE) {
                    delete(cache, &chat, &target)?;
                } else if authored
                    && protocol.r#type == Some(wa::message::protocol_message::Type::MESSAGE_EDIT)
                    && let Some(body) = protocol
                        .edited_message
                        .as_option()
                        .and_then(MessageExt::text_content)
                    && let Some(time) = protocol.timestamp_ms
                {
                    edit(cache, &chat, &target, body, time)?;
                }
            }
        }
        return Ok(());
    }
    let id = super::scoped_id(source.is_from_me, &sender, info.id.as_str());
    let attachments = if expiry.is_some_and(|seconds| seconds > 0) {
        Vec::new()
    } else {
        super::media::attachments(raw, &id)
    };
    let body = if raw.is_view_once() {
        "View-once message · open in WhatsApp"
    } else if expiry.is_some_and(|seconds| seconds > 0)
        || raw.is_ephemeral()
        || raw
            .get_ephemeral_expiration()
            .is_some_and(|seconds| seconds > 0)
    {
        "Disappearing message · open in WhatsApp"
    } else if let Some(body) = raw.text_content().or_else(|| raw.get_caption()) {
        body
    } else if !attachments.is_empty() {
        ""
    } else {
        return Ok(());
    };
    if raw.is_view_once()
        || raw.is_ephemeral()
        || expiry.is_some_and(|value| value > 0)
        || raw
            .get_ephemeral_expiration()
            .is_some_and(|value| value > 0)
    {
        let state = cache
            .whatsapp
            .rooms
            .entry(chat.clone())
            .or_default()
            .entry(id.clone())
            .or_default();
        state.restricted = Some(body.into());
        state.edit = None;
    }
    let reply_to = raw.context_info().and_then(|context| {
        let participant = context.participant.as_deref()?;
        Some(super::scoped_id(
            own_sender(participant, own),
            participant,
            context.stanza_id.as_deref()?,
        ))
    });
    let message = ChatMessage {
        content_state: Some(messages_core::MessageContentState::Available),
        id: id.clone(),
        transaction: None,
        sender,
        sender_name: info.push_name.to_string(),
        own: source.is_from_me,
        body: body.into(),
        timestamp: info.timestamp,
        delivery: if source.is_from_me {
            Delivery::Sent
        } else {
            Delivery::Received
        },
        edited: false,
        deleted: false,
        reply_to,
        reactions: Vec::new(),
        attachments,
    };
    if !source.is_group && !source.is_from_me {
        super::profiles::push_name(cache, &chat, info.push_name.as_str());
    }
    merge(
        cache,
        &chat,
        if source.is_group || source.is_from_me || info.push_name.is_empty() {
            &chat
        } else {
            info.push_name.as_str()
        },
        if source.is_group {
            RoomKind::Group
        } else {
            RoomKind::Direct
        },
        message,
    )
}

pub(super) fn merge(
    cache: &mut Cache,
    chat: &str,
    title: &str,
    kind: RoomKind,
    mut message: ChatMessage,
) -> anyhow::Result<()> {
    apply_state(
        cache
            .whatsapp
            .rooms
            .get(chat)
            .and_then(|room| room.get(&message.id)),
        &mut message,
    );
    let read_self = cache
        .whatsapp
        .rooms
        .get(chat)
        .and_then(|room| room.get(&message.id))
        .is_some_and(|state| state.read_self)
        || (!message.own
            && super::remote_id(&message.id).is_ok_and(|id| {
                cache
                    .whatsapp
                    .read_self_ids
                    .get(chat)
                    .is_some_and(|ids| ids.contains(id.as_str()))
            }));
    let through = message.timestamp.timestamp_millis();
    cache.upsert(
        chat,
        title,
        kind,
        Encryption::EndToEnd,
        super::capabilities(),
        message,
    )?;
    super::profiles::repair_room(cache, chat);
    if read_self {
        cache
            .store
            .mark_chat_read(&RoomKey::new(uuid::Uuid::nil(), chat), through);
    }
    Ok(())
}

fn apply_state(state: Option<&EventState>, message: &mut ChatMessage) {
    let Some(state) = state else {
        return;
    };
    if state.deleted {
        message.deleted = true;
        message.body.clear();
        message.reply_to = None;
        message.attachments.clear();
        message.reactions.clear();
        return;
    }
    if let Some(placeholder) = &state.restricted {
        message.body = placeholder.clone();
        message.edited = false;
    } else if let Some((_, text)) = &state.edit {
        message.body = text.clone();
        message.edited = true;
    }
    let mut grouped = BTreeMap::<&str, (u32, bool)>::new();
    for annotation in state
        .reactions
        .values()
        .filter(|annotation| !annotation.text.is_empty())
    {
        let group = grouped.entry(&annotation.text).or_default();
        group.0 += 1;
        group.1 |= annotation.own;
    }
    message.reactions = grouped
        .into_iter()
        .map(|(emoji, (count, own))| Reaction {
            emoji: emoji.into(),
            count,
            own,
        })
        .collect();
    if message.own {
        match state.receipt {
            2 if message.delivery != Delivery::Read => message.delivery = Delivery::Delivered,
            3 => message.delivery = Delivery::Read,
            _ => {}
        }
    }
}

fn update(cache: &mut Cache, chat: &str, id: &str) -> anyhow::Result<()> {
    let key = RoomKey::new(uuid::Uuid::nil(), chat);
    if let Some(mut message) = cache
        .store
        .room(&key)
        .and_then(|room| room.messages.iter().find(|message| message.id == id))
        .cloned()
    {
        apply_state(
            cache.whatsapp.rooms.get(chat).and_then(|room| room.get(id)),
            &mut message,
        );
        cache.store.merge_messages(&key, vec![message])?;
    }
    Ok(())
}

pub(super) fn annotate(
    cache: &mut Cache,
    chat: &str,
    target: &str,
    actor: &str,
    text: &str,
    time: i64,
    own: bool,
) -> anyhow::Result<()> {
    if text.encode_utf16().count() > 30 {
        return Ok(());
    }
    let state = cache
        .whatsapp
        .rooms
        .entry(chat.into())
        .or_default()
        .entry(target.into())
        .or_default();
    if !state.deleted && state.reactions.get(actor).is_none_or(|old| old.time < time) {
        state.reactions.insert(
            actor.into(),
            Annotation {
                time,
                text: text.into(),
                own,
            },
        );
    }
    update(cache, chat, target)
}

pub(super) fn edit(
    cache: &mut Cache,
    chat: &str,
    target: &str,
    body: &str,
    time: i64,
) -> anyhow::Result<()> {
    let state = cache
        .whatsapp
        .rooms
        .entry(chat.into())
        .or_default()
        .entry(target.into())
        .or_default();
    if !state.deleted
        && state.restricted.is_none()
        && state.edit.as_ref().is_none_or(|(old, _)| *old < time)
    {
        state.edit = Some((time, body.into()));
    }
    update(cache, chat, target)
}

pub(super) fn delete(cache: &mut Cache, chat: &str, target: &str) -> anyhow::Result<()> {
    let state = cache
        .whatsapp
        .rooms
        .entry(chat.into())
        .or_default()
        .entry(target.into())
        .or_default();
    state.deleted = true;
    state.edit = None;
    state.reactions.clear();
    update(cache, chat, target)
}

pub(super) fn timestamp(seconds: Option<u64>) -> Option<DateTime<Utc>> {
    seconds
        .and_then(|seconds| i64::try_from(seconds).ok())
        .and_then(|seconds| DateTime::from_timestamp(seconds, 0))
}
