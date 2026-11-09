use chrono::Utc;
use iced::{Task, widget::text_editor};
use messages_core::{ConnectionState, Reaction, RoomKey};
use messages_protocols::MessageAction;
use uuid::Uuid;

use super::{App, Message};

impl App {
    pub(super) fn send(&mut self) -> Task<Message> {
        if self.stopping() {
            return Task::none();
        }
        if self.loading || self.persistence_blocked {
            self.notice = Some("Messages cannot be sent until local storage is ready.".into());
            return Task::none();
        }
        let Some(key) = self.selected.clone() else {
            return Task::none();
        };
        if self.file_busy.contains(&key) {
            self.notice = Some("Wait for the attachments to finish preparing.".into());
            return Task::none();
        }
        let body = self.editor.text();
        let attachments = self
            .store
            .room(&key)
            .map(|room| room.draft_attachments.clone())
            .unwrap_or_default();
        if body.trim().is_empty() && attachments.is_empty() {
            return Task::none();
        }
        if let Some(message) = self.editing.clone() {
            let task = self.dispatch_action(key.clone(), MessageAction::Edit { message, body });
            return task;
        }
        match self
            .store
            .queue_with_attachments(&key, body, attachments, Utc::now())
        {
            Ok(request) => {
                self.editor = text_editor::Content::new();
                let stop = self.stop_typing(&key);
                Task::batch([stop, self.stage_send(request.transaction)])
            }
            Err(error) => {
                self.notice = Some(error.to_string().into());
                Task::none()
            }
        }
    }

    pub(super) fn stage_send(&mut self, transaction: Uuid) -> Task<Message> {
        if self.closing.is_some()
            || self.stop_mode.as_ref().is_some_and(|plan| {
                plan.choice == enzui_iced_core::activation::lifecycle::StopChoice::CancelAndExit
            })
        {
            return Task::none();
        }
        // Persist the in-flight marker before any transport may observe this
        // transaction. A crash between dispatch and ACK then recovers as uncertain.
        match self.store.begin_send(transaction) {
            Ok(_) => {
                self.send_admissions
                    .insert(transaction, self.jobs.begin(super::jobs::Kind::Send));
                self.dispatch_after_save.push(transaction);
                self.changed();
                self.save()
            }
            Err(error) => {
                self.notice = Some(error.to_string().into());
                Task::none()
            }
        }
    }

    pub(super) fn dispatch_send(&mut self, transaction: Uuid) -> Task<Message> {
        let Some(request) = self
            .store
            .outbox
            .iter()
            .find(|item| {
                item.request.transaction == transaction
                    && item.state == messages_core::OutboxState::Sending
            })
            .map(|item| item.request.clone())
        else {
            self.send_admissions.remove(&transaction);
            return Task::none();
        };
        let key = request.room.clone();
        let completion = self
            .send_admissions
            .remove(&transaction)
            .unwrap_or_else(|| self.jobs.begin(super::jobs::Kind::Send));
        let session = self.session(key.account);
        if let Err(error) = self
            .store
            .capabilities(&key)
            .require(messages_core::Capability::SendText)
        {
            self.store
                .fail_send(&key, transaction, messages_core::SendError::Rejected(error));
            self.changed();
            return self.save();
        }
        let sample = self.store.accounts.iter().any(|account| {
            account.id == key.account && account.connection == ConnectionState::Demo
        });
        if sample {
            return self.track_completion(
                completion,
                async move { Ok(format!("sample-{}", request.transaction)) },
                move |result| session.wrap(Message::Sent(key.clone(), transaction, result)),
            );
        }
        let Some(adapter) = self.adapters.get(&key.account).cloned() else {
            self.store.fail_send(
                &key,
                transaction,
                messages_core::SendError::Rejected(
                    "Reconnect this account in Settings to send.".into(),
                ),
            );
            self.changed();
            return self.save();
        };
        self.track_completion(
            completion,
            async move { adapter.send(&request).await },
            move |result| session.wrap(Message::Sent(key.clone(), transaction, result)),
        )
    }

    pub(super) fn dispatch_action(&mut self, key: RoomKey, action: MessageAction) -> Task<Message> {
        if let Err(reason) = self.store.capabilities(&key).require(action.capability()) {
            self.notice = Some(reason.into());
            return Task::none();
        }
        let close = self.close_dialog();
        if self
            .store
            .accounts
            .iter()
            .any(|account| account.id == key.account && account.connection == ConnectionState::Demo)
        {
            self.apply_action(&key, action);
            self.changed();
            return close;
        }
        let Some(adapter) = self.adapters.get(&key.account).cloned() else {
            self.notice = Some("Reconnect this account in Settings first.".into());
            return close;
        };
        let target = key.clone();
        let operation = action.clone();
        let session = self.session(key.account);
        Task::batch([
            close,
            self.track_job(
                super::jobs::Kind::RemoteAction,
                async move { adapter.action(&target, operation).await },
                move |result| {
                    session.wrap(Message::ActionDone(key.clone(), action.clone(), result))
                },
            ),
        ])
    }

    pub(super) fn apply_action(&mut self, key: &RoomKey, action: MessageAction) {
        if let MessageAction::Edit { message, body } = &action
            && self.selected.as_ref() == Some(key)
            && self
                .editing
                .as_ref()
                .is_some_and(|editing| editing.id == message.id)
            && self.editor.text() == *body
        {
            self.editing = None;
            self.editor = text_editor::Content::with_text(
                self.store
                    .room(key)
                    .map(|room| room.draft.as_str())
                    .unwrap_or(""),
            );
        }
        if self.store.accounts.iter().any(|account| {
            account.id == key.account
                && account.protocol.as_str() == "signal"
                && account.connection != ConnectionState::Demo
        }) {
            let timestamp = |id: &str| {
                self.store
                    .next_change_timestamp(key, id, Utc::now().timestamp_millis())
            };
            let actor = self
                .store
                .accounts
                .iter()
                .find(|account| account.id == key.account)
                .map(|account| account.user_id.clone())
                .unwrap_or_default();
            let change = match &action {
                MessageAction::React {
                    message,
                    emoji,
                    remove,
                } => Some((
                    message.id.clone(),
                    messages_core::ChangeKind::Reaction {
                        timestamp: timestamp(&message.id),
                        actor,
                        emoji: emoji.clone(),
                        remove: *remove,
                        own: true,
                    },
                )),
                MessageAction::Edit { message, body } if message.own => Some((
                    message.id.clone(),
                    messages_core::ChangeKind::Edit {
                        timestamp: timestamp(&message.id),
                        body: body.clone(),
                    },
                )),
                MessageAction::Delete { message } if message.own => {
                    Some((message.id.clone(), messages_core::ChangeKind::Delete))
                }
                _ => None,
            };
            if let Some((message, kind)) = change {
                self.store.apply_change(messages_core::MessageChange {
                    account: key.account,
                    room: Some(key.room.clone()),
                    message,
                    kind,
                });
                return;
            }
        }
        let Some(room) = self.store.room_mut(key) else {
            return;
        };
        match action {
            MessageAction::React {
                message,
                emoji,
                remove,
            } => {
                if let Some(target) = room
                    .messages
                    .iter_mut()
                    .find(|target| target.id == message.id)
                {
                    if let Some(reaction) = target
                        .reactions
                        .iter_mut()
                        .find(|reaction| reaction.emoji == emoji)
                    {
                        if remove && reaction.own {
                            reaction.count = reaction.count.saturating_sub(1);
                            reaction.own = false;
                        } else if !remove && !reaction.own {
                            reaction.count = reaction.count.saturating_add(1);
                            reaction.own = true;
                        }
                    } else if !remove {
                        target.reactions.push(Reaction {
                            emoji,
                            count: 1,
                            own: true,
                        });
                    }
                    target.reactions.retain(|reaction| reaction.count > 0);
                }
            }
            MessageAction::Edit { message, body } => {
                if let Some(target) = room
                    .messages
                    .iter_mut()
                    .find(|target| target.id == message.id && target.own)
                {
                    target.body = body;
                    target.edited = true;
                }
            }
            MessageAction::Delete { message } => {
                if let Some(target) = room
                    .messages
                    .iter_mut()
                    .find(|target| target.id == message.id && target.own)
                {
                    target.body.clear();
                    target.deleted = true;
                    target.reactions.clear();
                    target.attachments.clear();
                }
            }
            MessageAction::Read { .. } => room.unread = 0,
            MessageAction::Typing { .. } => {}
        }
    }
}
