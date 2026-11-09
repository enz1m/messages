use serde::{Deserialize, Serialize};

use crate::{AccountId, ChatMessage, Delivery, MessageStore, Reaction, RoomKey};

#[derive(Clone, Debug)]
pub struct MessageChange {
    pub account: AccountId,
    pub room: Option<String>,
    pub message: String,
    pub kind: ChangeKind,
}

#[derive(Clone, Debug)]
pub enum ChangeKind {
    Edit {
        timestamp: i64,
        body: String,
    },
    Delete,
    Reaction {
        timestamp: i64,
        actor: String,
        emoji: String,
        remove: bool,
        own: bool,
    },
    Receipt(Delivery),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MessageRevision {
    account: AccountId,
    room: Option<String>,
    message: String,
    edit: Option<(i64, String)>,
    deleted: bool,
    reactions: Vec<ActorReaction>,
    receipt: Option<Delivery>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ActorReaction {
    actor: String,
    timestamp: i64,
    emoji: Option<String>,
    own: bool,
}

impl MessageRevision {
    pub(crate) fn belongs_to(&self, key: &RoomKey) -> bool {
        self.account == key.account && self.room.as_ref() == Some(&key.room)
    }

    fn applies(&self, key: &RoomKey, message: &ChatMessage) -> bool {
        self.account == key.account
            && self.message == message.id
            && self.room.as_ref().is_none_or(|room| room == &key.room)
    }

    fn apply(&self, message: &mut ChatMessage) {
        if self.deleted || message.deleted {
            message.deleted = true;
            message.body.clear();
            message.attachments.clear();
            message.reactions.clear();
            message.reply_to = None;
            return;
        }
        if let Some((_, body)) = &self.edit {
            message.body.clone_from(body);
            message.edited = true;
        }
        if !self.reactions.is_empty() {
            message.reactions.clear();
            for actor in &self.reactions {
                let Some(emoji) = &actor.emoji else {
                    continue;
                };
                if let Some(reaction) = message
                    .reactions
                    .iter_mut()
                    .find(|reaction| &reaction.emoji == emoji)
                {
                    reaction.count = reaction.count.saturating_add(1);
                    reaction.own |= actor.own;
                } else {
                    message.reactions.push(Reaction {
                        emoji: emoji.clone(),
                        count: 1,
                        own: actor.own,
                    });
                }
            }
        }
        if let Some(receipt) = &self.receipt
            && message.own
            && receipt.receipt_rank() > message.delivery.receipt_rank()
        {
            message.delivery = receipt.clone();
        }
    }
}

impl MessageStore {
    pub fn next_change_timestamp(&self, key: &RoomKey, message: &str, clock: i64) -> i64 {
        self.revisions
            .iter()
            .filter(|revision| {
                revision.account == key.account
                    && revision.room.as_ref() == Some(&key.room)
                    && revision.message == message
            })
            .flat_map(|revision| {
                revision.edit.iter().map(|(time, _)| *time).chain(
                    revision
                        .reactions
                        .iter()
                        .filter(|reaction| reaction.own)
                        .map(|reaction| reaction.timestamp),
                )
            })
            .fold(clock, |time, old| time.max(old.saturating_add(1)))
    }

    pub fn forget_revisions(&mut self, account: AccountId) {
        self.revisions
            .retain(|revision| revision.account != account);
    }

    pub fn apply_change(&mut self, change: MessageChange) {
        if !self
            .accounts
            .iter()
            .any(|account| account.id == change.account)
        {
            return;
        }
        let index = self.revisions.iter().position(|revision| {
            revision.account == change.account
                && revision.room == change.room
                && revision.message == change.message
        });
        let index = index.unwrap_or_else(|| {
            self.revisions.push(MessageRevision {
                account: change.account,
                room: change.room.clone(),
                message: change.message,
                edit: None,
                deleted: false,
                reactions: Vec::new(),
                receipt: None,
            });
            self.revisions.len() - 1
        });
        let revision = &mut self.revisions[index];
        match change.kind {
            ChangeKind::Edit { timestamp, body } => {
                if !revision.deleted
                    && revision
                        .edit
                        .as_ref()
                        .is_none_or(|(version, _)| timestamp > *version)
                {
                    revision.edit = Some((timestamp, body));
                }
            }
            ChangeKind::Delete => {
                revision.deleted = true;
                revision.edit = None;
                revision.reactions.clear();
            }
            ChangeKind::Reaction {
                timestamp,
                actor,
                emoji,
                remove,
                own,
            } => {
                if !revision.deleted {
                    if let Some(old) = revision
                        .reactions
                        .iter_mut()
                        .find(|entry| entry.actor == actor)
                    {
                        if timestamp > old.timestamp {
                            // An older removal must not erase the actor's newer, different reaction.
                            if !remove || old.emoji.as_ref().is_none_or(|current| current == &emoji)
                            {
                                old.emoji = (!remove).then_some(emoji);
                            }
                            old.timestamp = timestamp;
                            old.own = own;
                        }
                    } else {
                        revision.reactions.push(ActorReaction {
                            actor,
                            timestamp,
                            emoji: (!remove).then_some(emoji),
                            own,
                        });
                    }
                }
            }
            ChangeKind::Receipt(delivery) => {
                if revision
                    .receipt
                    .as_ref()
                    .is_none_or(|old| delivery.receipt_rank() > old.receipt_rank())
                {
                    revision.receipt = Some(delivery);
                }
            }
        }
        for room in &mut self.conversations {
            for message in &mut room.messages {
                if revision.applies(&room.key, message) {
                    revision.apply(message);
                }
            }
        }
    }

    pub(crate) fn reapply_changes(&mut self, key: &RoomKey) {
        let revisions = &self.revisions;
        if let Some(room) = self.conversations.iter_mut().find(|room| &room.key == key) {
            for message in &mut room.messages {
                for revision in revisions {
                    if revision.applies(key, message) {
                        revision.apply(message);
                    }
                }
            }
        }
    }
}
