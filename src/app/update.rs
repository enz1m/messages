use enzui_iced_theme::EnzThemeMode;
use iced::{Task, widget::text_editor};
use messages_core::ConnectionState;

use super::{App, Dialog, Message};

impl App {
    pub fn update(&mut self, message: Message) -> Task<Message> {
        if self.stop_mode.is_some() && !message.allowed_while_stopping() {
            return Task::none();
        }
        let browser_before = self.login.as_ref().and_then(|login| login.browser.clone());
        let selected_before = self.selected.clone();
        let reconcile = message.changes_transcript_content();
        let task = self.reduce(message);
        self.present_notice();
        let transcript = self.maintain_transcript(selected_before != self.selected, reconcile);
        let avatars = self.save_avatars();
        let browser_cleanup = self.browser_cleanup(browser_before);
        if self.dialog != Some(Dialog::ConnectAccount) {
            self.platform_menu = enzui_iced_core::menu_model::MenuState::closed();
        }
        self.refresh_context_menu();
        let call_ui = if self.calls.present && !self.stopping() {
            self.calls.present = false;
            self.open_dialog(Dialog::Call)
        } else {
            Task::none()
        };
        let cleanup = self.call_cleanup();
        let stop = self.advance_stop();
        self.publish_lifecycle();
        browser_cleanup.chain(Task::batch([
            task, call_ui, cleanup, stop, transcript, avatars,
        ]))
    }

    pub(super) fn reduce(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::WorkCompleted(completion, message) => {
                completion.finish();
                let task = self.reduce(*message);
                // A settled job may be the last one holding an erasure barrier.
                return Task::batch([task, self.drive_account_erasure()]);
            }
            Message::WorkChanged => {}
            Message::LifecycleWindow(request, window) => {
                return self.lifecycle_window(request, window);
            }
            Message::ActivationFinished => {}
            Message::Shortcut(event, captured) => {
                let scope = enzui_iced_core::action::KeyRouteScope::new(
                    captured,
                    self.dialog.is_some()
                        || self.popup.is_some()
                        || self.context_menu.is_open()
                        || self.chooser.is_pending()
                        || self.chooser_start.active.is_some(),
                );
                if let Some(command) = enzui_iced_core::dispatch_shortcut_command(
                    self.shortcuts.resolved(),
                    &event,
                    &scope,
                ) {
                    return self.reduce(command);
                }
            }
            Message::ShortcutSettings(event) => self.shortcut_settings(event),
            Message::ShowShortcuts => {
                let task = self.open_dialog(Dialog::Settings);
                self.settings_page = "shortcuts".into();
                self.settings_drilled_in = true;
                return task;
            }
            Message::ToggleBubblePicker(side) => {
                self.bubble_picker_open = (self.bubble_picker_open != Some(side)).then_some(side);
            }
            Message::Emoji(event) => return self.emoji_event(event),
            Message::Devices(event) => return self.device_event(event),
            Message::Call(event) => return self.call_event(event),
            Message::NotificationsReady(result) => {
                self.notification_connecting = false;
                self.notification_retry = std::time::Instant::now();
                self.notifications = result.ok();
            }
            Message::NotificationWake => {
                let events = self
                    .notifications
                    .as_ref()
                    .map(|client| client.drain())
                    .unwrap_or_default();
                return Task::batch(
                    events
                        .into_iter()
                        .map(|event| self.notification_event(event)),
                );
            }
            Message::TogglePopup(popup) => {
                self.emoji_query.clear();
                self.emoji_generation = uuid::Uuid::new_v4();
                let close = self.close_dialog();
                self.popup = if self.popup.as_ref() == Some(&popup) {
                    None
                } else {
                    Some(popup)
                };
                if matches!(self.popup, Some(super::Popup::Accounts)) {
                    return close;
                }
                if self.popup.is_some() {
                    let prefix = super::emoji::picker_prefix(self.emoji_generation);
                    return close.chain(iced::advanced::widget::operate(
                        enzui_iced_core::focus::focus_and_reveal_id(
                            enzui_iced_widgets::emoji_picker::search_id(&prefix),
                        ),
                    ));
                }
            }
            Message::ClosePopup => {
                let focus = match self.popup.take() {
                    Some(super::Popup::Mute(_)) if self.dialog == Some(Dialog::Details) => {
                        "messages-details-mute"
                    }
                    Some(super::Popup::Mute(_)) => "messages-mute",
                    Some(super::Popup::Accounts) => "messages-account-overflow",
                    _ => "messages-composer",
                };
                return iced::advanced::widget::operate(
                    enzui_iced_core::focus::focus_and_reveal_id(iced::advanced::widget::Id::new(
                        focus,
                    )),
                );
            }
            Message::PickReaction(emoji) => return self.pick_reaction(emoji),
            Message::SharedSection(section) => self.shared_section = section,
            Message::RevealShared(id) => {
                let Some(key) = self.selected.as_ref() else {
                    return Task::none();
                };
                let target = iced::advanced::widget::Id::from(format!("messages-action-{id}"));
                if !self.store.room(key).is_some_and(|room| {
                    room.messages
                        .iter()
                        .any(|item| item.id == id && !item.deleted)
                }) {
                    return Task::none();
                }
                return self.close_dialog().chain(iced::advanced::widget::operate(
                    enzui_iced_core::focus::reveal_id_all(target),
                ));
            }
            Message::AvatarVisibility(session, key, shown) => {
                return self.avatar_visibility(session, key, shown);
            }
            Message::AvatarReady(session, key, revision, result) => {
                return self.avatar_ready(session, key, revision, result);
            }
            Message::PreviewVisible(key) => {
                self.visible_previews.insert(key);
                return self.load_previews();
            }
            Message::PreviewHidden(key) => {
                self.visible_previews.remove(&key);
            }
            Message::PreviewReady(session, key, result) => {
                return self.preview_ready(session, key, result);
            }
            Message::FilesDiscarded | Message::PresenceFinished => {}
            Message::PresenceReadFailed(key, message) => {
                self.read_sent
                    .retain(|(room, _), id| room != &key || id != &message);
            }
            Message::WindowFocused(focused) => {
                self.window_focused = focused;
                if focused {
                    self.quiet_foreground_notifications();
                    self.mark_visible_read();
                }
                if !focused && let Some(key) = self.selected.clone() {
                    return self.stop_typing(&key);
                }
            }
            Message::DeleteConversation(key) => return self.delete_conversation(key),
            Message::AttachFiles => return self.attach_files(),
            Message::FilesDropped(session, room, drop) => {
                return self.dropped_files(session, room, drop);
            }
            Message::DropStaged(token, session, room, result) => {
                let staged = self.drop_staged(token, session, room, result);
                let close = self
                    .closing
                    .map(|id| self.request_close(id))
                    .unwrap_or_else(Task::none);
                return Task::batch([staged, close]);
            }
            Message::RemoveAttachment(id) => return self.remove_attachment(id),
            Message::ChooserEvent(event) => {
                return self.chooser.handle(event, Message::ChooserDone);
            }
            Message::ChooserWake => return self.chooser.pump(Message::ChooserDone),
            Message::ChooserWindow(token, window) => return self.chooser_window(token, window),
            Message::ChooserNativeParent(token, window, parent) => {
                return self.chooser_native_parent(token, window, parent);
            }
            Message::ChooserExported(token, window, reply) => {
                return self.chooser_exported(token, window, reply);
            }
            Message::ChooserDone(purpose, outcome) => return self.chooser_done(purpose, outcome),
            Message::FilesStaged(session, key, result) => {
                let staged = self.files_staged(session, key, result);
                let close = self
                    .closing
                    .map(|id| self.request_close(id))
                    .unwrap_or_else(Task::none);
                return Task::batch([staged, close]);
            }
            Message::OpenAttachment(key, message, file) => {
                return self.open_attachment(key, message, file);
            }
            Message::SaveAttachment(key, message, file) => {
                return self.save_attachment(key, message, file);
            }
            Message::FileFinished(purpose, result) => {
                self.file_operations.remove(&purpose);
                self.file_tasks.remove(&purpose);
                if self.session_is_current(purpose.session())
                    && let Err(error) = result
                {
                    self.notice = Some(error.into());
                }
                if let Some(id) = self.closing {
                    return self.request_close(id);
                }
            }
            Message::ComposerMeasured(key, height) => {
                if self.selected.as_ref() == Some(&key)
                    && height.is_finite()
                    && (height - self.composer_height).abs() > 0.5
                {
                    self.composer_height = height.clamp(60.0, 500.0);
                }
            }
            Message::JumpToBottom(key) => {
                if self.selected.as_ref() == Some(&key) {
                    return self.jump_to_bottom(&key);
                }
            }
            Message::Preference(value) => return self.set_preference(value),
            Message::MuteFor(key, duration) => return self.mute_for(key, duration),
            Message::SelectChatItem(key, id) => self.select_message(key, id),
            Message::ClearMessageSelection => self.selected_messages.clear(),
            Message::CopySelected => return self.copy_selected(),
            Message::ThreadName(name) => self.thread_name = name,
            Message::StartThread(parent) => return self.start_thread(parent),
            Message::ThreadStarted(request, parent, result) => {
                return self.thread_started(request, parent, result);
            }
            Message::ToggleRoomBranch(key) => {
                if !self.collapsed_rooms.insert(key.clone()) {
                    self.collapsed_rooms.remove(&key);
                }
            }
            Message::AccountFilter(id) => {
                self.popup = None;
                self.account_filter = id;
                self.show_detail = false;
            }
            Message::SettingsPage(page) => {
                self.shortcut_armed = None;
                self.settings_page = page;
                self.settings_drilled_in = true;
                if self.settings_page == "devices" {
                    return self.device_event(super::devices::Event::Refresh);
                }
                self.devices.stop();
            }
            Message::SettingsOverview => {
                self.settings_drilled_in = false;
                self.devices.stop();
            }
            Message::WindowResized(size) => {
                self.overflow_open = false;
                self.sidebar_overflow_open = false;
                self.popup = None;
                self.window_size = size;
                if !self.loading {
                    self.mark_visible_read();
                }
            }
            Message::Context(target, point) => return self.open_context(target, point),
            Message::ContextReaction(generation, emoji) => {
                return self.context_reaction(generation, emoji);
            }
            Message::ContextEvent(event) => return self.context_event(event),
            Message::LoadOlder(key) => return self.load_older(key),
            Message::HistoryLoaded(key, before, result) => {
                return self.history_loaded(key, before, result);
            }
            Message::TranscriptScrolled(key, viewport) => {
                return self.transcript_scrolled(key, viewport);
            }
            Message::CredentialsReady(credentials) => {
                self.credentials = Some(credentials);
                return Task::batch([self.restore_accounts(), self.credential_tasks()]);
            }
            Message::CredentialLoaded(session, result) => {
                return self.credential_loaded(session, result);
            }
            Message::CredentialStored(session, key, result) => {
                return self.credential_stored(session, key, result);
            }
            Message::CredentialDeleted(key, result) => return self.credential_deleted(key, result),
            Message::RetryCredentials => {
                self.credential_failed.clear();
                self.credential_restoring.clear();
                return Task::batch([self.restore_accounts(), self.credential_tasks()]);
            }
            Message::SessionResult(session, message) => {
                if self.session_is_current(session) {
                    return self.reduce(*message);
                }
            }
            Message::Theme(event) => {
                if let enzui_iced_core::theme_file::integration::ThemeEvent::Loaded(load) =
                    event.as_ref()
                    && load.generation >= self.theme_writer.last_load
                {
                    self.theme_writer.last_load = load.generation;
                    if !self.theme_writer.active && self.theme_writer.pending.is_none() {
                        match &load.outcome {
                            Ok(document) => {
                                self.dark_override = match document.mode_preference() {
                                    enzui_iced_core::theme_file::ModePreference::Light => {
                                        Some(false)
                                    }
                                    enzui_iced_core::theme_file::ModePreference::Dark => Some(true),
                                    enzui_iced_core::theme_file::ModePreference::System => None,
                                }
                            }
                            Err(enzui_iced_core::theme_file::ThemeLoadError::Absent) => {
                                self.dark_override = None
                            }
                            Err(_) => {}
                        }
                    }
                }
                return self
                    .themes
                    .update(*event)
                    .map(|event| Message::Theme(Box::new(event)));
            }
            Message::Launched => {
                let (_, raise) = self.instance.drain(None);
                return Task::batch([raise, self.lifecycle_wake()]);
            }
            Message::ToggleSidebarOverflow => {
                self.sidebar_overflow_open = !self.sidebar_overflow_open
            }
            Message::DismissSidebarOverflow => self.sidebar_overflow_open = false,
            Message::ToggleOverflow => self.overflow_open = !self.overflow_open,
            Message::DismissOverflow => self.overflow_open = false,
            Message::Window(command) => {
                if matches!(
                    command,
                    enzui_iced_widgets::title_bar_gesture::WindowCommand::Close
                ) {
                    return iced::window::latest()
                        .and_then(|id| Task::done(Message::CloseRequested(id)));
                }
                return iced::window::latest()
                    .then(move |id| id.map_or_else(Task::none, |id| command.into_task(id)));
            }
            Message::CloseRequested(id) => {
                if self.stop_mode.is_none() {
                    return self.request_close(id);
                }
            }
            Message::AccountErased(account, sweep, result) => {
                return self.account_erased(account, sweep, result);
            }
            Message::ServicesStopped(id) => {
                self.services_stopping = false;
                if self.closing != Some(id) {
                    return Task::none();
                }
                return self.request_close(id);
            }
            Message::Sidebar(state) => self.nav = state,
            Message::SidebarScrolled(viewport) => {
                self.sidebar_scroll_offset = viewport.absolute_offset().y;
            }
            Message::Filter(filter) => {
                self.overflow_open = false;
                self.sidebar_overflow_open = false;
                self.popup = None;
                self.context_menu.close();
                self.filter = filter;
                self.show_detail = false;
                self.nav.overlay_open = false;
            }
            Message::Select(key) => {
                self.overflow_open = false;
                self.sidebar_overflow_open = false;
                let stop = self
                    .selected
                    .clone()
                    .map(|old| self.stop_typing(&old))
                    .unwrap_or_else(Task::none);
                self.popup = None;
                self.context_menu.close();
                self.selected_messages.clear();
                self.selected = Some(key.clone());

                self.show_detail = true;
                self.editing = None;
                if let Some(room) = self.store.room_mut(&key) {
                    self.editor = text_editor::Content::with_text(&room.draft);
                }
                self.mark_visible_read();
                self.changed();
                self.search_hit = self.search_hits().last().map(|message| message.id.clone());
                if self.dialog == Some(Dialog::NewConversation) {
                    return Task::batch([stop, self.focus_composer_after_dialog()]);
                }
                return Task::batch([stop, self.reveal_search_hit(), self.load_previews()]);
            }
            Message::Back => {
                self.overflow_open = false;
                self.sidebar_overflow_open = false;
                self.popup = None;
                self.show_detail = false;
            }
            Message::Search(query) => {
                self.popup = None;
                self.search = query;
                self.search_hit = None;
                self.show_detail = false;
            }
            Message::SearchStep(direction) => return self.step_search(direction),
            Message::FocusSearch => {
                self.nav.preference = enzui_iced_widgets::nav_split::SidebarPreference::Shown;
                self.nav.overlay_open = true;
                if self.window_size.width < 760.0 {
                    self.show_detail = false;
                }
                return iced::advanced::widget::operate(
                    enzui_iced_core::focus::focus_and_reveal_id(iced::advanced::widget::Id::new(
                        "messages-search",
                    )),
                );
            }
            Message::ComposerScrollBoundary => return Task::none(),
            Message::FocusComposer => {
                return iced::advanced::widget::operate(
                    enzui_iced_core::focus::focus_and_reveal_id(iced::advanced::widget::Id::new(
                        "messages-composer",
                    )),
                );
            }
            Message::Editor(action) => {
                let edited = action.is_edit();
                self.editor.perform(action);
                if !edited {
                    return Task::none();
                }
                if self.editing.is_none()
                    && let Some(room) = self
                        .selected
                        .as_ref()
                        .and_then(|key| self.store.room_mut(key))
                {
                    room.draft = self.editor.text();
                }
                self.changed();
                return self.composer_typing();
            }
            Message::Send => return self.send(),
            Message::Retry(transaction) => {
                return self.stage_send(transaction);
            }
            Message::Sent(key, transaction, result) => {
                match result {
                    Ok(remote) => self.store.acknowledge(&key, transaction, remote),
                    Err(error) => self.store.fail_send(
                        &key,
                        transaction,
                        if error.delivery_is_uncertain() {
                            messages_core::SendError::Uncertain
                        } else {
                            messages_core::SendError::Rejected(error.to_string())
                        },
                    ),
                }
                self.changed();
                return self.save();
            }
            Message::Action(key, action) => return self.dispatch_action(key, action),
            Message::ActionDone(key, action, result) => {
                if let Err(error) = result {
                    self.notice = Some(error.to_string().into());
                } else {
                    self.apply_action(&key, action);
                    self.changed();
                }
            }
            Message::Open(dialog) => return self.open_dialog(dialog),
            Message::CloseDialog => return self.close_dialog(),
            Message::Modal(event) => {
                let (closed, focus) = self.dialogs.update(event);
                if !closed.is_empty() {
                    self.cancel_service();
                    self.new_request = None;
                    self.new_busy = false;
                    self.dialog = self.dialogs.topmost().cloned();
                    self.login = None;
                }
                return focus;
            }
            Message::Reply(id) => {
                if let Some(room) = self
                    .selected
                    .as_ref()
                    .and_then(|key| self.store.room_mut(key))
                {
                    room.reply_to = Some(id);
                }
                self.editing = None;
                self.changed();
                return self.focus_composer_after_dialog();
            }
            Message::Edit(id) => {
                if let Some(message) = self
                    .current_room()
                    .and_then(|room| {
                        room.messages
                            .iter()
                            .find(|message| message.id == id && message.own)
                    })
                    .cloned()
                {
                    self.editor = text_editor::Content::with_text(&message.body);
                    self.editing = Some(message);
                }
                return self.focus_composer_after_dialog();
            }
            Message::CancelReply => {
                self.editing = None;
                if let Some(room) = self
                    .selected
                    .as_ref()
                    .and_then(|key| self.store.room_mut(key))
                {
                    room.reply_to = None;
                    self.editor = text_editor::Content::with_text(&room.draft);
                }
                self.changed();
            }
            Message::Copy(text) => {
                return Task::batch([
                    iced::clipboard::write(text).map(|result| {
                        Message::ClipboardWritten(result.map_err(|error| format!("{error:?}")))
                    }),
                    self.close_dialog(),
                ]);
            }
            Message::ClipboardWritten(result) => {
                if let Err(error) = result {
                    self.notice = Some(format!("Could not copy the message: {error}").into());
                }
            }
            Message::InsertEmoji(emoji) => {
                self.popup = None;
                self.editor
                    .perform(text_editor::Action::Edit(text_editor::Edit::Paste(
                        std::sync::Arc::new(emoji),
                    )));
                if self.editing.is_none()
                    && let Some(room) = self
                        .selected
                        .as_ref()
                        .and_then(|key| self.store.room_mut(key))
                {
                    room.draft = self.editor.text();
                }
                self.changed();
                return self.focus_composer_after_dialog();
            }
            Message::Pin => {
                if let Some(room) = self
                    .selected
                    .as_ref()
                    .and_then(|key| self.store.room_mut(key))
                {
                    room.pinned = !room.pinned;
                }
                self.changed();
            }
            Message::Mute => {
                if let Some(room) = self.current_room() {
                    return if room.muted {
                        self.mute_for(room.key.clone(), Some(0))
                    } else {
                        let target = super::Popup::Mute(room.key.clone());
                        self.popup = if self.popup.as_ref() == Some(&target) {
                            None
                        } else {
                            Some(target)
                        };
                        Task::none()
                    };
                }
            }
            Message::Archive => {
                if let Some(room) = self
                    .selected
                    .as_ref()
                    .and_then(|key| self.store.room_mut(key))
                {
                    room.archived = !room.archived;
                }
                self.show_detail = false;
                self.changed();
                return self.close_dialog();
            }
            Message::PlatformMenu(event) => {
                if self.dialog == Some(Dialog::ConnectAccount)
                    && let Some(id) = enzui_iced_widgets::menu_button::apply_event(
                        &mut self.platform_model,
                        &mut self.platform_menu,
                        &event,
                    )
                {
                    self.account_field(super::AccountField::Protocol(id.as_str().into()));
                }
            }
            Message::AccountField(field) => self.account_field(field),
            Message::Connect => return self.connect_account(),
            Message::ServiceLogin(session, result) => return self.restored_login(session, result),
            Message::ServicePrepared(generation, result) => {
                return self.service_prepared(generation, result);
            }
            Message::ServiceRestored(session, result) => {
                return self.service_restored(session, result);
            }
            Message::Reconnect(id) => return self.reconnect(id),
            Message::RemoveAccount(id) => return self.remove_account(id),
            Message::NewAccount(id) => self.new_account = Some(id),
            Message::NewRecipient(value) => self.new_recipient = value,
            Message::StartConversation => return self.start_conversation(),
            Message::ConversationStarted(request, id, result) => {
                return self.conversation_started(request, id, result);
            }
            Message::LoginLoad(id) => return self.load_login(id),
            Message::MatrixRecovery(id) => return self.restore_matrix_keys(id),
            Message::LoginLoaded(showing, id, result) => {
                return self.login_loaded(showing, id, result);
            }
            Message::LoginValue(value) => {
                if let Some(login) = &mut self.login {
                    login.value = value.expose().into();
                }
            }
            Message::LoginUser(value) => {
                if let Some(login) = &mut self.login {
                    login.user = value;
                }
            }
            Message::LoginSubmit => return self.submit_login(),
            Message::BrowserBegin => return self.begin_browser(),
            Message::BrowserCreated(completion) => return self.browser_created(*completion),
            Message::BrowserOpen => return self.open_browser(),
            Message::BrowserCancel => return self.cancel_browser(),
            Message::BrowserOpened(showing, id, result) => self.browser_opened(showing, id, result),
            Message::LoginSubmitted(showing, id, result) => {
                if let Some(login) = self
                    .login
                    .as_mut()
                    .filter(|login| login.account == id && login.showing == showing)
                {
                    login.busy = false;
                    if let Err(error) = result {
                        login.error = Some(error.to_string());
                    } else if matches!(
                        login.state,
                        messages_protocols::login::LoginState::MatrixRecovery
                    ) {
                        self.login = None;
                        return Task::batch([self.close_dialog(), self.sync_account(id)]);
                    } else {
                        return self.load_login(id);
                    }
                }
            }
            Message::SampleSaved(result) => {
                if let Err(error) = result {
                    self.notice = Some(error.into());
                }
            }
            Message::Synced(account, result) => {
                self.sync_finished(account, result);
            }
            Message::TranscriptViewport(key, size) => return self.transcript_viewport(key, size),
            Message::TranscriptMeasured(key, generation, row, measurement) => {
                return self.measure_transcript(key, generation, &row, measurement);
            }
            Message::RetryNotifications => {
                self.notification_retry =
                    std::time::Instant::now() - std::time::Duration::from_secs(61);
                return self.retry_notifications();
            }
            Message::TestNotification => {
                self.test_notification();
            }
            Message::Tick => {
                if self.stop_mode.is_some() {
                    return Task::none();
                }
                if let Some(window) = self.closing {
                    return self.request_close(window);
                }
                self.expire_mutes();
                let save = self.save();
                let authenticating = self.login.as_ref().map(|login| login.account);
                let ids: Vec<_> = self
                    .adapters
                    .keys()
                    .copied()
                    .filter(|id| {
                        !self.syncing.contains(id) && !self.managed_login_checks.contains_key(id)
                    })
                    .filter(|id| Some(*id) != authenticating)
                    .filter(|id| {
                        self.store.accounts.iter().any(|account| {
                            account.id == *id
                                && !matches!(
                                    account.connection,
                                    ConnectionState::NeedsAuthentication(_)
                                )
                        })
                    })
                    .collect();
                let mut tasks = vec![
                    save,
                    self.presence_tick(),
                    self.load_previews(),
                    self.load_avatars(),
                    self.retry_notifications(),
                ];
                let restoring: Vec<_> = self.managed_login_checks.keys().copied().collect();
                for account in restoring {
                    tasks.push(self.restore_login(account));
                }
                if let Some(super::Dialog::AccountLogin(id)) = self.dialog {
                    tasks.push(self.load_login(id));
                }
                for id in ids {
                    tasks.push(self.sync_account(id));
                }
                return Task::batch(tasks);
            }
            Message::ThemeSaved(result) => return self.theme_saved(result),
            Message::SetTheme(dark) => {
                self.dark_override = dark;
                self.themes.set_mode_override(dark.map(|dark| {
                    if dark {
                        EnzThemeMode::Dark
                    } else {
                        EnzThemeMode::Light
                    }
                }));
                self.changed();
                return self.queue_theme(dark, false);
            }
            Message::DismissNotice => self.notice = None,
            Message::NoticeToast(event) => {
                for id in self.notice_toasts.update(event) {
                    // An expired or closed toast dismisses the notice it presented.
                    if self
                        .notice_toast
                        .as_ref()
                        .is_some_and(|(shown, _)| *shown == id)
                    {
                        self.notice_toast = None;
                        self.notice = None;
                    }
                }
                self.notice_toasts.tick_at(std::time::Instant::now());
            }
            Message::WorkspaceLoaded(result, photos) => {
                self.avatars.restore(photos);
                return self.reduce(Message::Loaded(result));
            }
            Message::AvatarCacheSaved(revision) => {
                self.avatars.saved(revision);
                return self.save_avatars();
            }
            Message::Loaded(result) => {
                self.loading = false;
                self.scroll_positions.clear();
                match result {
                    Ok(Some(snapshot)) => {
                        if !self.theme_writer.chosen
                            && let Some(dark) = snapshot.dark
                        {
                            self.theme_writer.pending = Some(super::theme_preferences::Change {
                                mode: Some(dark),
                                migrate: true,
                            });
                        }
                        self.preferences = snapshot.preferences;
                        self.restore_shortcuts();
                        self.store = snapshot.store;
                        self.notification_since.clear();
                        self.notification_seen = self
                            .store
                            .conversations
                            .iter()
                            .filter_map(|room| {
                                room.messages
                                    .iter()
                                    .rev()
                                    .find(|message| !message.own && !message.deleted)
                                    .map(|m| (room.key.clone(), m.id.clone()))
                            })
                            .collect();
                        self.demo = snapshot.demo;
                        self.connections = snapshot.connections;
                        self.credential_keys = snapshot.credential_keys;
                        self.credential_cleanup = snapshot.credential_cleanup;
                        self.account_purges = snapshot.account_purges;
                        self.selected = snapshot
                            .selected
                            .filter(|key| self.store.room(key).is_some())
                            .or_else(|| {
                                self.store
                                    .conversations
                                    .first()
                                    .map(|room| room.key.clone())
                            });
                        if self.current_room().is_some_and(|room| room.archived) {
                            self.filter = super::Filter::Archived;
                        }
                        self.editor = text_editor::Content::with_text(
                            self.current_room()
                                .map(|room| room.draft.as_str())
                                .unwrap_or(""),
                        );
                    }
                    Ok(None) => {}
                    Err(error) => {
                        self.notice = Some(error.into());
                        self.persistence_blocked = true;
                    }
                }
                self.restore_signal_read_state();
                self.mark_visible_read();
                let theme = self.pump_theme();
                if let Some(id) = self.closing {
                    return Task::batch([theme, self.request_close(id)]);
                }
                let sweep = self.sweep_account_erasures();
                return Task::batch([
                    theme,
                    sweep,
                    self.restore_accounts(),
                    self.save(),
                    self.credential_tasks(),
                ]);
            }
            Message::Saved(revision, result) => {
                self.saving = false;
                match result {
                    Ok(()) => {
                        self.saved_revision = self.saved_revision.max(revision);
                        self.settle_drops();
                    }
                    Err(error) => {
                        self.drop_admissions.clear();
                        self.stop_mode = None;
                        self.closing = None;
                        self.notice = Some(error.into());
                        return Task::none();
                    }
                }
                if self.revision > self.saved_revision {
                    return self.save();
                }
                let credentials = Task::batch([
                    self.credential_tasks(),
                    self.reclaim_files(),
                    self.drive_account_erasure(),
                ]);
                if let Some(id) = self.closing {
                    return Task::batch([credentials, self.request_close(id)]);
                }
                let requests = std::mem::take(&mut self.dispatch_after_save);
                let mut tasks = vec![credentials];
                for transaction in requests {
                    tasks.push(self.dispatch_send(transaction));
                }
                return Task::batch(tasks);
            }
        }
        Task::none()
    }
}
