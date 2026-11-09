use iced::Task;
use messages_core::{Account, ConnectionState, Conversation, Encryption, ProtocolId, RoomKind};
use messages_protocols::{ConnectionConfig, ManagedConnection, SyncBatch};
use uuid::Uuid;

use super::{AccountField, App, Dialog, Message};

impl App {
    pub(super) fn account_field(&mut self, field: AccountField) {
        if matches!(field, AccountField::MatrixServer(_))
            && (self.account_form.protocol != "matrix" || !self.account_form.managed)
        {
            return;
        }
        self.cancel_service();
        match field {
            AccountField::Protocol(id) => {
                if self.registry.descriptor(&id).is_none() {
                    return;
                }
                self.account_form.editing = None;
                self.account_form.user.clear();
                self.account_form.token.clear();
                self.account_form.rooms.clear();
                self.account_form.managed = true;
                self.account_form.remember_token = true;
                if let Some(descriptor) = self.registry.descriptor(&id) {
                    self.account_form.endpoint = descriptor.default_endpoint.into();
                }
                self.account_form.native_bridge = matches!(id.as_str(), "matrix" | "whatsapp");

                self.account_form.protocol = id;
                if self.account_form.protocol == "matrix" {
                    self.account_form.identify_matrix_server();
                }
            }
            AccountField::Managed(managed) => {
                self.account_form.managed = managed;
                self.account_form.endpoint = if self.account_form.protocol == "matrix" && managed {
                    "https://matrix.org"
                } else if self.account_form.protocol == "matrix" {
                    "http://127.0.0.1:8823"
                } else {
                    self.registry
                        .descriptor(&self.account_form.protocol)
                        .map(|descriptor| descriptor.default_endpoint)
                        .unwrap_or("")
                }
                .into();
                if managed && self.account_form.protocol == "matrix" {
                    self.account_form.identify_matrix_server();
                }
            }
            AccountField::MatrixServer(server) => self.account_form.select_matrix_server(server),
            AccountField::Endpoint(value) => {
                self.account_form.endpoint = value;
                if self.account_form.managed && self.account_form.protocol == "matrix" {
                    self.account_form.matrix_server = super::matrix_servers::Server::Custom;
                    self.account_form
                        .matrix_custom_endpoint
                        .clone_from(&self.account_form.endpoint);
                }
            }
            AccountField::User(value) => self.account_form.user = value,
            AccountField::Token(value) => self.account_form.token = value.expose().to_owned(),
            AccountField::RememberToken(remember) => self.account_form.remember_token = remember,
            AccountField::NativeBridge(native) => {
                self.account_form.native_bridge = native;
                self.account_form.endpoint = if native {
                    "http://127.0.0.1:8823"
                } else {
                    "https://matrix.org"
                }
                .into();
            }
        }
    }

    pub(super) fn reconnect(&mut self, id: Uuid) -> Task<Message> {
        if let Some(config) = self.connections.iter().find(|config| config.account == id) {
            self.account_form.editing = Some(id);
            self.account_form.protocol = config.protocol.as_str().into();
            self.account_form.endpoint = config.endpoint.clone();
            self.account_form.user = config.user_id.clone();
            self.account_form.rooms = config.rooms.join(", ");
            self.account_form.native_bridge = config.native_bridge;
            self.account_form.managed = config.managed.is_some();
            if let Some(ManagedConnection::Matrix { homeserver }) = &config.managed {
                self.account_form.endpoint = homeserver.clone();
                self.account_form.identify_matrix_server();
            }
            self.account_form.token.clear();
            self.account_form.remember_token = self.credential_keys.contains_key(&id);
        }
        self.open_dialog(Dialog::ConnectAccount)
    }

    pub(super) fn connect_account(&mut self) -> Task<Message> {
        if self.loading || self.persistence_blocked || self.saving || self.pending_service.is_some()
        {
            self.notice = Some("Wait for saved conversations to finish loading or saving.".into());
            return Task::none();
        }
        let form = &self.account_form;
        let id = form.editing.unwrap_or_else(Uuid::new_v4);
        let label = self
            .registry
            .descriptor(&form.protocol)
            .map(|d| d.name)
            .unwrap_or("Account")
            .to_owned();
        let config = ConnectionConfig {
            account: id,
            protocol: ProtocolId::new(form.protocol.clone()),
            label: label.clone(),
            endpoint: form.endpoint.trim().into(),
            user_id: form.user.trim().into(),
            rooms: form
                .rooms
                .split(',')
                .map(str::trim)
                .filter(|room| !room.is_empty())
                .map(str::to_owned)
                .collect(),
            bot: false,
            native_bridge: form.native_bridge,
            managed: if form.managed {
                match form.protocol.as_str() {
                    "signal" => Some(ManagedConnection::Signal),
                    "whatsapp" => Some(ManagedConnection::WhatsApp),
                    "matrix" => Some(ManagedConnection::Matrix {
                        homeserver: form.endpoint.trim().into(),
                    }),
                    _ => None,
                }
            } else {
                None
            },
        };
        let token = (!form.token.is_empty()).then(|| form.token.clone());
        let remember_token = form.remember_token;
        if config.managed.is_some() {
            return self.prepare_service(config);
        }
        self.install_connection(config, token, remember_token)
    }

    pub(super) fn install_connection(
        &mut self,
        config: ConnectionConfig,
        token: Option<String>,
        remember_token: bool,
    ) -> Task<Message> {
        let id = config.account;
        let label = config.label.clone();
        match self.registry.connect(config.clone(), token.clone()) {
            Ok(adapter) => {
                let sample_save = if self.demo {
                    let mut sample = self.store.clone();
                    sample
                        .accounts
                        .retain(|account| account.connection == ConnectionState::Demo);
                    let sample_ids: Vec<_> =
                        sample.accounts.iter().map(|account| account.id).collect();
                    sample
                        .conversations
                        .retain(|room| sample_ids.contains(&room.key.account));
                    sample
                        .outbox
                        .retain(|item| sample_ids.contains(&item.request.room.account));
                    let snapshot = crate::persistence::Snapshot {
                        version: 1,
                        preferences: self.preferences.clone(),
                        demo: true,
                        selected: self.selected.clone(),
                        dark: self.dark_override,
                        store: sample,
                        connections: Vec::new(),
                        credential_keys: Default::default(),
                        credential_cleanup: Vec::new(),
                        account_purges: Vec::new(),
                    };
                    self.store
                        .accounts
                        .retain(|account| account.connection != ConnectionState::Demo);
                    let ids: Vec<_> = self
                        .store
                        .accounts
                        .iter()
                        .map(|account| account.id)
                        .collect();
                    self.store
                        .conversations
                        .retain(|room| ids.contains(&room.key.account));
                    self.store
                        .outbox
                        .retain(|item| ids.contains(&item.request.room.account));
                    self.demo = false;
                    self.selected = None;
                    self.new_account = None;
                    self.new_busy = false;
                    self.editor = iced::widget::text_editor::Content::new();
                    self.dispatch_after_save.clear();
                    self.track_job(
                        super::jobs::Kind::Persistence,
                        crate::persistence::save(
                            self.storage_path.with_file_name("sample-state.json"),
                            snapshot,
                        ),
                        Message::SampleSaved,
                    )
                } else {
                    Task::none()
                };
                let account = Account {
                    id,
                    protocol: config.protocol.clone(),
                    label,
                    user_id: config.user_id.clone(),
                    connection: ConnectionState::Connecting,
                    capabilities: adapter.capabilities(),
                };
                if let Some(existing) = self
                    .store
                    .accounts
                    .iter_mut()
                    .find(|account| account.id == id)
                {
                    *existing = account;
                } else {
                    self.store.accounts.push(account);
                }
                self.renew_session(id);
                self.adapters.insert(id, adapter);
                self.stage_credential(id, token, remember_token);
                self.connections.retain(|config| config.account != id);
                self.cursors.remove(&id);
                let native = config.managed.is_some()
                    || config.native_bridge
                    || config.protocol.as_str() == "whatsapp";
                self.connections.push(config);
                self.account_form.token.clear();
                self.account_form.editing = None;
                self.notice = Some(super::notice::Notice::connecting(self.session(id)));
                self.changed();
                Task::batch([
                    sample_save,
                    self.save(),
                    if native {
                        self.load_login(id)
                    } else {
                        self.close_dialog().chain(self.sync_account(id))
                    },
                ])
            }
            Err(error) => {
                self.notice = Some(error.to_string().into());
                Task::none()
            }
        }
    }

    pub(super) fn sync_account(&mut self, id: Uuid) -> Task<Message> {
        if self.stopping() || !self.sync_due(id) {
            return Task::none();
        }
        let Some(adapter) = self.adapters.get(&id).cloned() else {
            return Task::none();
        };
        if !self.syncing.insert(id) {
            return Task::none();
        }
        let cursor = self.cursors.get(&id).cloned();
        let session = self.session(id);
        self.track_job(
            super::jobs::Kind::Sync,
            async move { adapter.sync(cursor.as_deref()).await },
            move |result| session.wrap(Message::Synced(id, result)),
        )
    }

    pub(super) fn apply_sync(&mut self, id: Uuid, batch: SyncBatch) {
        if let Some(user) = batch.user_id.as_ref()
            && self.store.accounts.iter().any(|account| {
                account.id == id && !account.user_id.is_empty() && account.user_id != *user
            })
        {
            self.renew_session(id);
            self.avatars.forget(id);
            self.adapters.remove(&id);
            let reason = "This connection belongs to a different account. Add it as a new account in Settings.";
            if let Some(account) = self
                .store
                .accounts
                .iter_mut()
                .find(|account| account.id == id)
            {
                account.connection = ConnectionState::Failed(reason.into());
            }
            self.notice = Some(reason.into());
            self.changed();
            return;
        }
        if let Some(cursor) = batch.cursor {
            self.cursors.insert(id, cursor);
        }
        if let Some(account) = self
            .store
            .accounts
            .iter_mut()
            .find(|account| account.id == id)
        {
            account.connection = ConnectionState::Online;
            if let Some(user) = batch.user_id {
                if let Some(config) = self
                    .connections
                    .iter_mut()
                    .find(|config| config.account == id)
                {
                    config.user_id = user.clone();
                }
                account.user_id = user;
            }
            if let Some(adapter) = self.adapters.get(&id) {
                account.capabilities = adapter.capabilities();
            }
        }
        for mut incoming in batch.conversations {
            incoming.draft.clear();
            incoming.draft_attachments.clear();
            if incoming.key.account != id {
                continue;
            }
            let key = incoming.key.clone();
            let known = self.store.room(&key).is_some();
            let messages = std::mem::take(&mut incoming.messages)
                .into_iter()
                .filter(|message| self.store.admits_message(&key, message))
                .collect::<Vec<_>>();
            if !known && self.store.chat_was_deleted(&key) && messages.is_empty() {
                continue;
            }
            if let Some(existing) = self.store.room_mut(&key) {
                existing.title = incoming.title;
                existing.avatar_revision = incoming.avatar_revision;
                existing.subtitle = incoming.subtitle;
                existing.group_name = incoming.group_name;
                existing.parent_room = incoming.parent_room;
                existing.kind = incoming.kind;
                existing.capabilities = incoming.capabilities;
                existing.encryption = incoming.encryption;
                if existing.subtitle != "Signal" || !incoming.members.is_empty() {
                    existing.members = incoming.members;
                }
                if existing.messages.is_empty() {
                    existing.has_older = incoming.has_older;
                    existing.pagination = incoming.pagination;
                }
            } else {
                self.store.conversations.push(incoming);
            }
            let _ = self.merge_live_messages(&key, messages);
        }
        for (key, messages) in batch.messages {
            let messages = messages
                .into_iter()
                .filter(|message| self.store.admits_message(&key, message))
                .collect::<Vec<_>>();
            if self.store.room(&key).is_none()
                && self.store.chat_was_deleted(&key)
                && messages.is_empty()
            {
                continue;
            }
            if key.account != id {
                continue;
            }
            let known = self.store.room(&key).is_some();
            if !known {
                let title = messages
                    .first()
                    .map(|message| message.sender_name.clone())
                    .unwrap_or_else(|| key.room.clone());
                let capabilities = self
                    .adapters
                    .get(&id)
                    .map(|adapter| adapter.capabilities())
                    .unwrap_or_default();
                self.store.conversations.push(Conversation {
                    key: key.clone(),
                    title,
                    subtitle: "Signal".into(),
                    kind: if key.room.starts_with("group:") {
                        RoomKind::Group
                    } else {
                        RoomKind::Direct
                    },
                    members: Vec::new(),
                    unread: 0,
                    pinned: false,
                    muted: false,
                    muted_until: None,
                    avatar_revision: 0,
                    group_name: None,
                    parent_room: None,
                    archived: false,
                    encryption: Encryption::EndToEnd,
                    capabilities,
                    messages: Vec::new(),
                    draft: String::new(),
                    draft_attachments: Vec::new(),
                    reply_to: None,
                    has_older: false,
                    pagination: None,
                });
            }
            let _ = self.merge_live_messages(&key, messages);
        }
        for change in batch.changes {
            if change.account == id {
                self.store.apply_change(change);
            }
        }
        for read in batch.device_reads {
            if read.account == id {
                self.store.apply_device_read(read);
            }
        }
        for (key, through) in batch.read_through {
            if key.account == id {
                self.store.mark_chat_read(&key, through);
                self.withdraw_room_notification(&key);
            }
        }
        for typing in batch.typing {
            if typing.room.account == id {
                let key = (typing.room, typing.actor);
                if typing.active {
                    self.typing
                        .insert(key, (typing.name, std::time::Instant::now()));
                } else {
                    self.typing.remove(&key);
                }
            }
        }
        if self.selected.is_none() {
            self.selected = self
                .store
                .conversations
                .first()
                .map(|room| room.key.clone());
        }
        self.mark_visible_read();
        self.notify_unread(id);
        self.clear_connection_notice(self.session(id));
        self.admit_call_invites(id, batch.calls);
        self.changed();
    }
    fn merge_live_messages(
        &mut self,
        key: &messages_core::RoomKey,
        messages: Vec<messages_core::ChatMessage>,
    ) -> Result<(), messages_core::StoreError> {
        let visible = self.window_focused
            && self.selected.as_ref() == Some(key)
            && (self.window_size.width >= 760.0 || self.show_detail);
        let additional = if visible {
            0
        } else {
            self.store
                .room(key)
                .map(|room| (room, room.messages.last().map(|last| last.timestamp)))
                .map(|(room, newest)| {
                    messages
                        .iter()
                        .filter(|message| {
                            !message.own
                                && !message.deleted
                                && !self.store.message_is_read(key, message)
                                && newest.is_none_or(|newest| message.timestamp > newest)
                                && !room.messages.iter().any(|old| old.id == message.id)
                        })
                        .map(|message| message.id.as_str())
                        .collect::<std::collections::HashSet<_>>()
                        .len()
                })
                .unwrap_or(0)
        };
        self.store.merge_messages(key, messages)?;
        if let Some(room) = self.store.room_mut(key) {
            if visible {
                room.unread = 0;
            } else {
                room.unread = room
                    .unread
                    .saturating_add(u32::try_from(additional).unwrap_or(u32::MAX));
            }
        }
        Ok(())
    }

    pub(super) fn mark_visible_read(&mut self) {
        if self.window_focused
            && (self.window_size.width >= 760.0 || self.show_detail)
            && let Some(key) = self.selected.clone()
            && let Some(room) = self.store.room(&key)
        {
            let through = room
                .messages
                .last()
                .map(|message| message.timestamp.timestamp_millis())
                .unwrap_or(0);
            let unread = room.unread;
            self.store.mark_chat_read(&key, through);
            self.withdraw_room_notification(&key);
            if unread > 0 {
                self.changed();
            }
        }
    }
}

pub(super) fn platform_model() -> enzui_iced_core::menu_model::MenuModel {
    use enzui_iced_core::menu_model::{MenuEntryModel, MenuModel};
    let registry = messages_protocols::Registry::default();
    let mut entries = registry
        .protocols()
        .map(|item| MenuEntryModel::command(item.id, item.name).icon(item.icon))
        .collect::<Vec<_>>();
    entries.extend([
        MenuEntryModel::command("discord", "Discord (soon)")
            .icon("gamepad-2")
            .enabled(false),
        MenuEntryModel::command("telegram", "Telegram (soon)")
            .icon("send")
            .enabled(false),
    ]);
    MenuModel::new(entries)
}
