use enzui_iced_core::context_menu::InvocationKind;
use enzui_iced_core::context_menu::Point;
use enzui_iced_widgets::context_menu_host::ContextMenuItem as Item;
use enzui_iced_widgets::context_menu_host::{ContextMenuEvent, FocusRestoreTarget};
use iced::Task;
use messages_core::Capability;
use messages_protocols::MessageAction;

use super::{App, ContextTarget, Message};

#[derive(Clone, Debug)]
pub enum Command {
    Open,
    Pin,
    MuteFor(Option<i64>),
    Archive,
    DeleteConversation,
    Thread,
    Copy,
    Reply,
    Edit,
    Delete,
    Select,
    Retry,
}

impl Command {
    fn key(&self) -> String {
        match self {
            Self::Open => "open",
            Self::Pin => "pin",
            Self::MuteFor(duration) => return format!("mute-{duration:?}"),
            Self::Archive => "archive",
            Self::DeleteConversation => "delete-conversation",
            Self::Thread => "thread",
            Self::Copy => "copy",
            Self::Reply => "reply",
            Self::Edit => "edit",
            Self::Delete => "delete",
            Self::Select => "select",
            Self::Retry => "retry",
        }
        .into()
    }
}

impl App {
    pub(super) fn open_context(
        &mut self,
        target: ContextTarget,
        point: iced::Point,
    ) -> Task<Message> {
        self.popup = None;
        let Some((entries, restore)) = self.context_entries(&target) else {
            return Task::none();
        };
        self.context_showing = uuid::Uuid::new_v4();
        self.context_menu.open_restoring(
            target,
            InvocationKind::Pointer(Point::new(point.x, point.y)),
            entries,
            restore,
        );
        Task::none()
    }

    fn context_entries(
        &self,
        target: &ContextTarget,
    ) -> Option<(Vec<Item<Command>>, FocusRestoreTarget)> {
        let (entries, restore) = match target {
            ContextTarget::Conversation(key) => {
                let room = self.store.room(key)?;
                let path = format!("{}:{}", key.account, key.room);
                let mut entries = vec![
                    Item::command(Command::Open, "Open conversation").icon("message-circle"),
                    Item::command(
                        Command::Pin,
                        if room.pinned {
                            "Unpin"
                        } else {
                            "Pin conversation"
                        },
                    )
                    .icon("pin"),
                    if room.muted {
                        Item::command(Command::MuteFor(Some(0)), "Unmute").icon("bell-off")
                    } else {
                        Item::submenu(
                            "Mute for…",
                            super::customization::MUTE_DURATIONS
                                .iter()
                                .map(|(label, duration)| {
                                    let command = Command::MuteFor(*duration);
                                    let key = command.key();
                                    Item::command(command, *label).key(key)
                                })
                                .collect(),
                        )
                        .key("mute")
                        .icon("bell-off")
                    },
                    Item::separator(),
                    Item::command(
                        Command::Archive,
                        if room.archived {
                            "Move to inbox"
                        } else {
                            "Archive conversation"
                        },
                    )
                    .icon("archive"),
                    Item::separator(),
                    Item::command(Command::DeleteConversation, "Delete conversation…")
                        .icon("trash-2")
                        .enabled(
                            !self
                                .store
                                .outbox
                                .iter()
                                .any(|item| &item.request.room == key),
                        ),
                ];
                if self.store.capabilities(key).supports(Capability::Threads) {
                    let label = "New thread…";
                    entries.insert(
                        1,
                        Item::command(Command::Thread, label).icon("messages-square"),
                    );
                }
                (
                    entries,
                    FocusRestoreTarget::tree_row_path("messages-conversations", &[&path]),
                )
            }
            ContextTarget::Message(key, id) => {
                let message = self.store.room(key).and_then(|room| {
                    room.messages
                        .iter()
                        .find(|message| &message.id == id && !message.deleted)
                })?;
                let caps = self.store.capabilities(key);
                let mut entries = vec![Item::command(Command::Copy, "Copy text").icon("copy")];
                if caps.supports(Capability::Reply) {
                    entries.push(Item::command(Command::Reply, "Reply").icon("reply"));
                }
                if message.own && caps.supports(Capability::EditOwn) {
                    entries.push(Item::command(Command::Edit, "Edit message").icon("pencil"));
                }
                if message.own && caps.supports(Capability::DeleteOwn) {
                    entries.push(Item::command(Command::Delete, "Delete message").icon("trash-2"));
                }
                entries.push(Item::command(Command::Select, "Select message").icon("circle-check"));
                if let (Some(_), messages_core::Delivery::Failed(_)) =
                    (message.transaction, &message.delivery)
                {
                    entries.push(Item::command(Command::Retry, "Retry sending").icon("refresh-cw"));
                }
                (
                    entries,
                    FocusRestoreTarget::widget(iced::advanced::widget::Id::new(
                        "messages-composer",
                    )),
                )
            }
        };
        let entries = entries
            .into_iter()
            .map(|item| {
                let key = match &item {
                    Item::Command { command, .. } => Some(command.key()),
                    _ => None,
                };
                match key {
                    Some(key) => item.key(key),
                    None => item,
                }
            })
            .collect();
        Some((entries, restore))
    }

    pub(super) fn refresh_context_menu(&mut self) {
        let Some(target) = self.context_menu.target().cloned() else {
            return;
        };
        if let Some((entries, _)) = self.context_entries(&target) {
            self.context_menu.reconcile(entries);
        } else {
            self.context_menu.close();
        }
    }

    pub(super) fn context_event(&mut self, event: ContextMenuEvent) -> Task<Message> {
        let (chosen, focus) = self.context_menu.update(&event);
        let action = chosen
            .map(|(target, command)| self.context_command(target, command))
            .unwrap_or_else(Task::none);
        focus.chain(action)
    }

    pub(super) fn context_reaction(&mut self, showing: uuid::Uuid, emoji: String) -> Task<Message> {
        if showing != self.context_showing {
            return Task::none();
        }
        let Some(ContextTarget::Message(key, id)) = self.context_menu.target().cloned() else {
            return Task::none();
        };
        let close = self.context_event(ContextMenuEvent::Dismissed(
            enzui_iced_core::menu_model::CloseCause::Activated,
        ));
        self.popup = Some(super::Popup::Reaction(key, id));
        let action = self.pick_reaction(emoji);
        close.chain(action)
    }

    pub(super) fn pick_reaction(&mut self, emoji: String) -> Task<Message> {
        let Some(super::Popup::Reaction(key, id)) = self.popup.take() else {
            return Task::none();
        };
        if self.selected.as_ref() != Some(&key)
            || !self.store.capabilities(&key).supports(Capability::React)
        {
            return Task::none();
        }
        let Some(message) = self
            .store
            .room(&key)
            .and_then(|room| {
                room.messages
                    .iter()
                    .find(|item| item.id == id && !item.deleted)
            })
            .cloned()
        else {
            return Task::none();
        };
        let remove = message
            .reactions
            .iter()
            .any(|item| item.emoji == emoji && item.own);
        self.update(Message::Action(
            key,
            MessageAction::React {
                message,
                emoji,
                remove,
            },
        ))
    }

    fn context_command(&mut self, target: ContextTarget, command: Command) -> Task<Message> {
        match target {
            ContextTarget::Conversation(key) => {
                if matches!(command, Command::DeleteConversation) {
                    return self.update(Message::Open(super::Dialog::DeleteConversation(key)));
                }
                if matches!(command, Command::Thread) {
                    return self.update(Message::Open(super::Dialog::NewThread(key)));
                }
                if let Command::MuteFor(duration) = command {
                    return self.mute_for(key, duration);
                }
                if matches!(command, Command::Open) {
                    return self.update(Message::Select(key));
                }
                if let Some(room) = self.store.room_mut(&key) {
                    match command {
                        Command::Pin => room.pinned = !room.pinned,
                        Command::Archive => room.archived = !room.archived,
                        _ => return Task::none(),
                    }
                    self.changed();
                }
            }
            ContextTarget::Message(key, id) => {
                let Some(message) = self
                    .store
                    .room(&key)
                    .and_then(|room| {
                        room.messages
                            .iter()
                            .find(|message| message.id == id && !message.deleted)
                    })
                    .cloned()
                else {
                    return Task::none();
                };
                match command {
                    Command::Copy => return self.update(Message::Copy(message.body)),
                    Command::Reply if self.selected.as_ref() == Some(&key) => {
                        return self.update(Message::Reply(id));
                    }
                    Command::Edit if self.selected.as_ref() == Some(&key) => {
                        return self.update(Message::Edit(id));
                    }
                    Command::Select => return self.update(Message::SelectChatItem(key, id)),
                    Command::Retry => {
                        if let Some(transaction) = message.transaction {
                            return self.update(Message::Retry(transaction));
                        }
                    }
                    Command::Delete => {
                        return self.update(Message::Open(super::Dialog::DeleteMessage(key, id)));
                    }
                    _ => return Task::none(),
                };
            }
        }
        Task::none()
    }
}
