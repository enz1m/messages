use messages_core::{Capability, Conversation, Delivery, Encryption, RoomKey, RoomKind};
use whatsapp_rust::{
    prelude::*,
    wacore::types::{
        events::LazyHistorySync,
        message::{MessageInfo, MessageSource},
    },
};

use super::events;
use crate::cache::Cache;

pub(super) fn import(
    cache: &mut Cache,
    history: &LazyHistorySync,
    own: &[String],
) -> anyhow::Result<()> {
    let mut stream = history.stream();
    while let Some(conversation) = stream.next_conversation()? {
        import_conversation(cache, &conversation, own)?;
    }
    anyhow::ensure!(
        stream.skipped_conversations() == 0,
        "Some WhatsApp history could not be decoded"
    );
    for name in stream.remainder()?.pushnames {
        if let (Some(jid), Some(name)) = (name.id, name.pushname) {
            super::profiles::push_name(cache, &jid, &name);
        }
    }
    super::profiles::repair(cache);
    Ok(())
}

fn source(
    key: &wa::MessageKey,
    chat: &Jid,
    participant: Option<&str>,
    own: &[String],
) -> Option<MessageSource> {
    let from_me = key.from_me?;
    let is_group = chat.to_string().ends_with("@g.us");
    let sender = if from_me {
        own.first()?.parse().ok()?
    } else if is_group {
        key.participant.as_deref().or(participant)?.parse().ok()?
    } else {
        chat.clone()
    };
    Some(MessageSource {
        chat: chat.clone(),
        sender,
        is_from_me: from_me,
        is_group,
        ..Default::default()
    })
}

fn import_conversation(
    cache: &mut Cache,
    conversation: &wa::Conversation,
    own: &[String],
) -> anyhow::Result<()> {
    let chat: Jid = conversation.id.parse()?;
    let chat_id = chat.to_string();
    if chat_id.ends_with("@broadcast") || chat_id.ends_with("@newsletter") {
        return Ok(());
    }
    let key = RoomKey::new(uuid::Uuid::nil(), &chat_id);
    let title = conversation
        .name
        .as_deref()
        .or(conversation.display_name.as_deref())
        .unwrap_or(&chat_id);
    if cache.store.room(&key).is_none() {
        cache.store.conversations.push(Conversation::empty(
            key.clone(),
            title,
            if chat_id.ends_with("@g.us") {
                RoomKind::Group
            } else {
                RoomKind::Direct
            },
            Encryption::EndToEnd,
            super::capabilities(),
        ));
    }
    if !chat_id.ends_with("@g.us") {
        super::profiles::history_name(cache, &chat_id, title);
    }
    if let Some(room) = cache.store.room_mut(&key) {
        if !title.trim().is_empty() && title != chat_id {
            room.title = title.into();
        }
        room.unread = conversation.unread_count.unwrap_or(room.unread);
        room.archived = conversation.archived.unwrap_or(room.archived);
        if conversation.read_only == Some(true) {
            room.capabilities
                .disable(Capability::SendText, "This conversation is read-only.");
        }
    }
    for history_message in &conversation.messages {
        let Some(web) = history_message.message.as_option() else {
            continue;
        };
        let Some(message_key) = web.key.as_option() else {
            continue;
        };
        let Some(source) = source(message_key, &chat, web.participant.as_deref(), own) else {
            continue;
        };
        let Some(id) = message_key
            .id
            .as_deref()
            .and_then(|id| MessageId::new(id).ok())
        else {
            continue;
        };
        let Some(timestamp) = events::timestamp(web.message_timestamp) else {
            continue;
        };
        let mut info = MessageInfo {
            source,
            id: id.as_str().parse()?,
            timestamp,
            push_name: web.push_name.as_deref().unwrap_or("").into(),
            ..Default::default()
        };
        let target = super::scoped_id(
            info.source.is_from_me,
            &info.source.sender.to_string(),
            info.id.as_str(),
        );
        if web.message_stub_type == Some(wa::web_message_info::StubType::REVOKE) {
            events::receive(
                cache,
                &wa::Message::text("Message deleted"),
                &info,
                own,
                None,
                true,
            )?;
            events::delete(cache, &chat_id, &target)?;
            continue;
        }
        let Some(message) = web.message.as_option() else {
            continue;
        };
        // Edited-only history entries carry the new content under protocolMessage;
        // hydrate the original identity before applying its edit timestamp.
        if let Some(protocol) = message.get_base_message().protocol_message.as_option()
            && protocol.r#type == Some(wa::message::protocol_message::Type::MESSAGE_EDIT)
            && let Some(edited) = protocol.edited_message.as_option()
            && let Some(id) = protocol
                .key
                .as_option()
                .and_then(|key| key.id.as_deref())
                .and_then(|id| MessageId::new(id).ok())
        {
            info.id = id.as_str().parse()?;
            events::receive(cache, edited, &info, own, web.ephemeral_duration, true)?;
        }
        events::receive(cache, message, &info, own, web.ephemeral_duration, true)?;
        let target = super::scoped_id(
            info.source.is_from_me,
            &info.source.sender.to_string(),
            info.id.as_str(),
        );
        if let Some(message) = cache.store.room_mut(&key).and_then(|room| {
            room.messages
                .iter_mut()
                .find(|message| message.id == target)
        }) && message.own
        {
            match web.status {
                Some(wa::web_message_info::Status::READ | wa::web_message_info::Status::PLAYED) => {
                    message.delivery = Delivery::Read
                }
                Some(wa::web_message_info::Status::DELIVERY_ACK)
                    if message.delivery != Delivery::Read =>
                {
                    message.delivery = Delivery::Delivered
                }
                _ => {}
            }
        }
        if web.message_add_ons.is_empty() {
            for reaction in &web.reactions {
                if let Some(reaction_key) = reaction.key.as_option() {
                    import_reaction(
                        cache,
                        &chat,
                        &target,
                        reaction_key,
                        reaction.text.as_deref(),
                        reaction.sender_timestamp_ms,
                        own,
                    )?;
                }
            }
        } else {
            for addon in &web.message_add_ons {
                if let Some(reaction) = addon
                    .message_add_on
                    .as_option()
                    .and_then(|message| message.reaction_message.as_option())
                    && let Some(reaction_key) = addon.message_add_on_key.as_option()
                {
                    import_reaction(
                        cache,
                        &chat,
                        &target,
                        reaction_key,
                        reaction.text.as_deref(),
                        reaction.sender_timestamp_ms,
                        own,
                    )?;
                }
            }
        }
    }
    Ok(())
}

fn import_reaction(
    cache: &mut Cache,
    chat: &Jid,
    target: &str,
    key: &wa::MessageKey,
    text: Option<&str>,
    time: Option<i64>,
    own: &[String],
) -> anyhow::Result<()> {
    if let Some(source) = source(key, chat, None, own)
        && let Some(time) = time
    {
        let sender = source.sender.to_string();
        events::annotate(
            cache,
            &chat.to_string(),
            target,
            if source.is_from_me { "own" } else { &sender },
            text.unwrap_or(""),
            time,
            source.is_from_me,
        )?;
    }
    Ok(())
}
