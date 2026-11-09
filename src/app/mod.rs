mod accounts;
mod avatars;
mod sync;
pub(crate) use lifecycle::Session;
mod browser_login;
pub(super) mod calls;
mod chooser;
mod context;
mod conversations;
mod credentials;
pub(super) mod customization;
pub(super) mod devices;
mod dialogs;
mod drops;
pub(super) mod emoji;
mod erasure;
mod files;
pub(super) mod history;
mod jobs;
mod lifecycle;
mod login;
pub(super) mod matrix_servers;
mod messaging;
mod new_conversation;
mod notice;
mod notifications;
mod os_lifecycle;
mod presence;
pub(super) mod previews;
mod search;
mod services;
pub(super) mod shortcuts;
#[cfg(test)]
mod tests;
mod theme_preferences;
mod threads;
pub(super) mod transcript;
mod update;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use enzui_iced_core::theme_file::integration::{ThemeEvent, ThemeIntegration};
use enzui_iced_theme::EnzThemeMode;
use enzui_iced_widgets::modal_host::{DialogEvent, DialogStack};
use enzui_iced_widgets::nav_split::NavSplitState;
use enzui_iced_widgets::title_bar_gesture::WindowCommand;
use iced::widget::text_editor;
use iced::{Subscription, Task};
use messages_core::{AccountId, ChatMessage, MessageStore, RoomKey};
use messages_protocols::{
    Adapter, AdapterError, ConnectionConfig, MessageAction, Registry, SyncBatch,
};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Filter {
    All,
    Unread,
    Archived,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Dialog {
    Settings,
    Call,
    ConnectAccount,
    NewConversation,
    NewThread(RoomKey),
    DeleteMessage(RoomKey, String),
    DeleteConversation(RoomKey),
    Details,
    AccountLogin(AccountId),
    RemoveAccount(AccountId),
    AccountSettings(AccountId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Popup {
    Emoji,
    Mute(RoomKey),
    Accounts,
    Reaction(RoomKey, String),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SharedSection {
    #[default]
    Media,
    Links,
    Files,
}

#[derive(Clone, Debug)]
pub enum ContextTarget {
    Conversation(RoomKey),
    Message(RoomKey, String),
}

#[derive(Clone, Debug)]
pub enum AccountField {
    Protocol(String),
    Endpoint(String),
    MatrixServer(matrix_servers::Server),
    User(String),
    Token(messages_protocols::login::Secret),
    NativeBridge(bool),
    RememberToken(bool),
    Managed(bool),
}

#[derive(Clone, Debug)]
pub enum Message {
    WorkCompleted(jobs::Completion, Box<Message>),
    WorkChanged,
    LifecycleWindow(
        enzui_iced_core::activation::lifecycle::StopRequest,
        Option<iced::window::Id>,
    ),
    NotificationsReady(Result<Arc<crate::platform::notifications::Client>, String>),
    NotificationWake,
    ActivationFinished,
    TogglePopup(Popup),
    ClosePopup,
    PickReaction(String),
    SharedSection(SharedSection),
    RevealShared(String),
    FilesDiscarded,
    PresenceFinished,
    PresenceReadFailed(RoomKey, String),
    WindowFocused(bool),
    ComposerMeasured(RoomKey, f32),
    PreviewVisible(previews::PreviewKey),
    PreviewHidden(previews::PreviewKey),
    PreviewReady(
        lifecycle::Session,
        previews::PreviewKey,
        Result<crate::platform::previews::Thumbnail, String>,
    ),
    AvatarVisibility(lifecycle::Session, RoomKey, bool),
    AvatarReady(
        lifecycle::Session,
        RoomKey,
        u64,
        Result<Option<iced::widget::image::Handle>, ()>,
    ),
    AttachFiles,
    DeleteConversation(RoomKey),
    RemoveAttachment(uuid::Uuid),
    ChooserWake,
    ChooserWindow(Uuid, Option<iced::window::Id>),
    ChooserNativeParent(
        Uuid,
        iced::window::Id,
        Option<enzui_iced_widgets::portal_parent::ParentExport>,
    ),
    ChooserExported(Uuid, iced::window::Id, chooser::ExportReply),
    ChooserEvent(enzui_iced_widgets::file_chooser::FallbackEvent),
    ChooserDone(
        chooser::Ticket,
        enzui_iced_widgets::file_chooser::PortalOutcome<
            enzui_iced_widgets::file_chooser::FileResult,
        >,
    ),
    FilesStaged(
        lifecycle::Session,
        RoomKey,
        Result<Vec<messages_core::OutgoingAttachment>, String>,
    ),
    FilesDropped(
        lifecycle::Session,
        RoomKey,
        enzui_iced_widgets::file_drop::FileDrop,
    ),
    DropStaged(
        Uuid,
        lifecycle::Session,
        RoomKey,
        Result<Vec<messages_core::OutgoingAttachment>, String>,
    ),
    OpenAttachment(RoomKey, String, String),
    SaveAttachment(RoomKey, String, String),
    FileFinished(files::Purpose, Result<(), String>),
    Context(ContextTarget, iced::Point),
    SettingsPage(String),
    SettingsOverview,
    AccountFilter(Option<AccountId>),
    ToggleRoomBranch(RoomKey),
    ThreadName(String),
    StartThread(RoomKey),
    ThreadStarted(
        Uuid,
        RoomKey,
        Result<messages_core::Conversation, AdapterError>,
    ),
    Preference(customization::Preference),
    MuteFor(RoomKey, Option<i64>),
    SelectChatItem(RoomKey, String),
    ClearMessageSelection,
    CopySelected,
    WindowResized(iced::Size),
    ContextReaction(Uuid, String),
    ContextEvent(enzui_iced_widgets::context_menu_host::ContextMenuEvent),
    LoadOlder(messages_core::RoomKey),
    HistoryLoaded(
        messages_core::RoomKey,
        Option<String>,
        Result<messages_protocols::HistoryPage, AdapterError>,
    ),
    TranscriptScrolled(messages_core::RoomKey, iced::widget::scrollable::Viewport),
    JumpToBottom(RoomKey),
    SessionResult(lifecycle::Session, Box<Message>),
    CredentialsReady(Arc<crate::platform::credentials::Credentials>),
    CredentialLoaded(
        lifecycle::Session,
        Result<
            Option<messages_protocols::login::Secret>,
            crate::platform::credentials::CredentialIssue,
        >,
    ),
    CredentialStored(
        lifecycle::Session,
        crate::platform::credentials::CredentialRef,
        Result<(), crate::platform::credentials::CredentialIssue>,
    ),
    CredentialDeleted(
        crate::platform::credentials::CredentialRef,
        Result<(), crate::platform::credentials::CredentialIssue>,
    ),
    RetryCredentials,
    Theme(Box<ThemeEvent>),
    Shortcut(iced::keyboard::Event, bool),
    ShortcutSettings(enzui_iced_widgets::editable_shortcuts::EditableShortcutsEvent),
    ShowShortcuts,
    ToggleBubblePicker(customization::BubbleSide),
    Emoji(emoji::Event),
    Devices(devices::Event),
    Call(calls::Event),
    Launched,
    ToggleOverflow,
    ToggleSidebarOverflow,
    DismissSidebarOverflow,
    DismissOverflow,
    Window(WindowCommand),
    CloseRequested(iced::window::Id),
    ServicesStopped(iced::window::Id),
    /// An account's storage erasure settled: the account, whether it was the
    /// start-up sweep, and the outcome.
    AccountErased(AccountId, bool, Result<(), String>),
    Sidebar(NavSplitState),
    SidebarScrolled(iced::widget::scrollable::Viewport),
    Filter(Filter),
    Select(RoomKey),
    Back,
    Search(String),
    SearchStep(i8),
    FocusSearch,
    FocusComposer,
    ComposerScrollBoundary,
    Editor(text_editor::Action),
    Send,
    Retry(Uuid),
    Sent(RoomKey, Uuid, Result<String, AdapterError>),
    Action(RoomKey, MessageAction),
    ActionDone(RoomKey, MessageAction, Result<(), AdapterError>),
    Open(Dialog),
    CloseDialog,
    Modal(DialogEvent<Dialog>),
    Reply(String),
    Edit(String),
    CancelReply,
    Copy(String),
    ClipboardWritten(Result<(), String>),
    InsertEmoji(String),
    Pin,
    Mute,
    Archive,
    AccountField(AccountField),
    PlatformMenu(enzui_iced_widgets::menu_button::MenuButtonEvent),
    Connect,
    ServiceLogin(
        lifecycle::Session,
        Result<messages_protocols::login::LoginState, AdapterError>,
    ),
    ServicePrepared(Uuid, Result<crate::platform::services::Prepared, String>),
    ServiceRestored(
        lifecycle::Session,
        Result<crate::platform::services::Prepared, String>,
    ),
    Reconnect(AccountId),
    RemoveAccount(AccountId),
    NewAccount(AccountId),
    NewRecipient(String),
    StartConversation,
    ConversationStarted(
        Uuid,
        AccountId,
        Result<messages_core::Conversation, AdapterError>,
    ),
    LoginLoad(AccountId),
    MatrixRecovery(AccountId),
    LoginLoaded(
        Uuid,
        AccountId,
        Result<messages_protocols::login::LoginState, AdapterError>,
    ),
    LoginValue(messages_protocols::login::Secret),
    LoginUser(String),
    LoginSubmit,
    BrowserBegin,
    BrowserCancel,
    BrowserOpen,
    BrowserCreated(Box<browser_login::Completion>),
    BrowserOpened(Uuid, AccountId, Result<(), String>),
    LoginSubmitted(Uuid, AccountId, Result<(), AdapterError>),
    SampleSaved(Result<(), String>),
    Synced(AccountId, Result<SyncBatch, AdapterError>),
    TranscriptViewport(RoomKey, iced::Size),
    /// A transcript row's measured height, with the layout generation it was
    /// rendered in and the row's key ("header", "m:<message id>" or "footer").
    TranscriptMeasured(
        RoomKey,
        uuid::Uuid,
        String,
        enzui_iced_widgets::row_measure::RowMeasurement,
    ),
    TestNotification,
    RetryNotifications,
    Tick,
    SetTheme(Option<bool>),
    ThemeSaved(Result<(), String>),
    DismissNotice,
    // The transient notice's toast: hover, close and expiry.
    NoticeToast(enzui_iced_widgets::toast_host::ToastEvent),
    Loaded(Result<Option<crate::persistence::Snapshot>, String>),
    WorkspaceLoaded(
        Result<Option<crate::persistence::Snapshot>, String>,
        Vec<crate::platform::avatar_cache::Photo>,
    ),
    AvatarCacheSaved(u64),
    Saved(u64, Result<(), String>),
}

pub struct AccountForm {
    pub editing: Option<AccountId>,
    pub protocol: String,
    pub endpoint: String,
    pub matrix_server: matrix_servers::Server,
    pub matrix_custom_endpoint: String,
    pub user: String,
    pub token: String,
    pub rooms: String,
    pub native_bridge: bool,
    pub remember_token: bool,
    pub managed: bool,
}

impl Default for AccountForm {
    fn default() -> Self {
        Self {
            editing: None,
            protocol: "matrix".into(),
            endpoint: "https://matrix.org".into(),
            matrix_server: matrix_servers::Server::default(),
            matrix_custom_endpoint: String::new(),
            user: String::new(),
            token: String::new(),
            rooms: String::new(),
            native_bridge: true,
            remember_token: true,
            managed: true,
        }
    }
}

pub struct App {
    pub themes: ThemeIntegration,
    pub window_size: iced::Size,
    pub window_focused: bool,
    pub settings_page: String,
    pub devices: devices::State,
    pub calls: calls::State,
    pub settings_drilled_in: bool,
    pub popup: Option<Popup>,
    pub shared_section: SharedSection,
    pub store: MessageStore,
    pub demo: bool,
    pub nav: NavSplitState,
    pub sidebar_scroll_offset: f32,
    pub selected: Option<RoomKey>,
    pub show_detail: bool,
    pub filter: Filter,
    pub account_filter: Option<AccountId>,
    pub selected_messages: HashSet<String>,
    pub collapsed_rooms: HashSet<RoomKey>,
    pub composer_height: f32,
    pub search: String,
    pub search_hit: Option<String>,
    pub editor: text_editor::Content,
    pub editing: Option<ChatMessage>,
    pub dialog: Option<Dialog>,
    pub dialogs: DialogStack<Dialog>,
    pub confirmation: enzui_iced_core::confirm::ConfirmState,
    pub instance: enzui_iced_widgets::single_instance::SingleInstance,
    pub jobs: jobs::Registry,
    pub send_admissions: HashMap<Uuid, jobs::Completion>,
    pub stop_control: Option<enzui_iced_core::activation::lifecycle::StopControl>,
    pub stop_mode: Option<os_lifecycle::Plan>,
    pub boot_nonce: Uuid,
    pub overflow_open: bool,
    pub sidebar_overflow_open: bool,
    pub notice: Option<notice::Notice>,
    // Transient (Info) notices show as a toast that expires; the toast
    // presenting `notice`, if any, and the expiry timer it arms.
    pub notice_toasts: enzui_iced_widgets::toast_host::ToastHost,
    pub notice_toast: Option<(enzui_iced_widgets::toast_host::ToastId, notice::Notice)>,
    pub notice_expiry: enzui_iced_widgets::toast_host::ToastExpirySchedule,
    pub context_menu:
        enzui_iced_widgets::context_menu_host::ContextMenu<ContextTarget, context::Command>,
    pub context_showing: Uuid,
    pub account_form: AccountForm,
    pub avatars: avatars::Avatars,
    pub platform_model: enzui_iced_core::menu_model::MenuModel,
    pub platform_menu: enzui_iced_core::menu_model::MenuState,
    pub pending_service: Option<Uuid>,
    pub service_task: Option<iced::task::Handle>,
    pub services_stopping: bool,
    pub managed_login_checks: HashMap<AccountId, std::time::Instant>,
    pub managed_hosts: HashMap<AccountId, Arc<crate::platform::services::Host>>,
    pub new_account: Option<AccountId>,
    pub new_recipient: String,
    pub new_busy: bool,
    pub new_request: Option<(Uuid, AccountId)>,
    pub thread_name: String,
    pub login: Option<login::Login>,
    pub registry: Registry,
    pub connections: Vec<ConnectionConfig>,
    pub adapters: HashMap<AccountId, Arc<dyn Adapter>>,
    pub sessions: HashMap<AccountId, Uuid>,
    pub history_busy: HashSet<RoomKey>,
    pub history_errors: HashMap<RoomKey, String>,
    pub scroll_positions: HashMap<RoomKey, history::Position>,
    pub cursors: HashMap<AccountId, String>,
    pub syncing: HashSet<AccountId>,
    pub sync_retry: HashMap<AccountId, sync::Retry>,
    pub loading: bool,
    pub persistence_blocked: bool,
    pub revision: u64,
    pub saved_revision: u64,
    pub saving: bool,
    pub dispatch_after_save: Vec<Uuid>,
    pub dark_override: Option<bool>,
    theme_writer: theme_preferences::Writer,
    pub closing: Option<iced::window::Id>,
    pub chooser: enzui_iced_widgets::file_chooser::ChooserSession<chooser::Ticket>,
    pub chooser_start: chooser::Controller,
    pub drop_admissions: HashMap<Uuid, drops::Admission>,
    pub typing: HashMap<(RoomKey, String), (String, std::time::Instant)>,
    pub typing_sent: HashMap<RoomKey, (std::time::Instant, std::time::Instant)>,
    pub read_sent: HashMap<(RoomKey, String), String>,
    pub preferences: crate::persistence::Preferences,
    pub shortcuts:
        enzui_iced_core::EditableShortcutCatalog<Message, enzui_iced_core::action::KeyRouteScope>,
    pub shortcut_armed: Option<String>,
    pub shortcut_feedback: Option<(String, String)>,
    pub bubble_picker_open: Option<customization::BubbleSide>,
    pub emoji_query: String,
    pub emoji_category: enzui_iced_widgets::emoji_picker::EmojiCategory,
    pub emoji_recents: enzui_iced_widgets::emoji_picker::EmojiRecents,
    pub emoji_generation: Uuid,
    pub notifications: Option<Arc<crate::platform::notifications::Client>>,
    pub notification_retry: std::time::Instant,
    pub notification_connecting: bool,
    pub notification_pending: HashMap<Uuid, notifications::Binding>,
    pub notification_bindings: HashMap<u32, notifications::Binding>,
    pub notification_activations: HashMap<u32, enzui_iced_core::activation::ActivationContext>,
    pub notification_seen: HashMap<RoomKey, String>,
    pub notification_since: HashMap<AccountId, chrono::DateTime<chrono::Utc>>,
    pub file_busy: HashSet<RoomKey>,
    pub file_reclaims: Vec<(u64, RoomKey, messages_core::OutgoingAttachment)>,
    pub file_tasks: HashMap<files::Purpose, iced::task::Handle>,
    pub file_operations: HashSet<files::Purpose>,
    pub preview_pool: enzui_iced_core::blocking_task::BlockingTaskOwner,
    pub previews: HashMap<previews::PreviewKey, previews::Preview>,
    pub visible_previews: HashSet<previews::PreviewKey>,
    pub preview_tasks: HashMap<previews::PreviewKey, iced::task::Handle>,
    pub storage_path: std::path::PathBuf,
    pub credentials: Option<Arc<crate::platform::credentials::Credentials>>,
    pub credential_keys: std::collections::BTreeMap<Uuid, Uuid>,
    pub credential_cleanup: Vec<crate::platform::credentials::CredentialRef>,
    /// Durable tombstones of removed accounts whose storage may still exist.
    pub account_purges: Vec<AccountId>,
    /// In-session erasures waiting for the removal snapshot at this revision.
    pub erase_after: Vec<(u64, AccountId)>,
    /// Helpers of removed accounts, stopped by their erasure (or at close).
    pub erase_hosts: HashMap<AccountId, Arc<crate::platform::services::Host>>,
    pub erasing: HashSet<AccountId>,
    pub credential_pending: Vec<credentials::PendingStore>,
    pub credential_busy: HashSet<crate::platform::credentials::CredentialRef>,
    pub credential_failed: HashSet<crate::platform::credentials::CredentialRef>,
    pub credential_restoring: HashSet<AccountId>,
    pub credential_notice: Option<String>,
}

impl App {
    pub fn new() -> (Self, Task<Message>) {
        let (mut themes, boot) = ThemeIntegration::boot();
        let dark_override =
            std::env::var("MESSAGES_THEME")
                .ok()
                .and_then(|value| match value.as_str() {
                    "dark" => Some(true),
                    "light" => Some(false),
                    _ => None,
                });
        if let Some(dark) = dark_override {
            themes.set_mode_override(Some(if dark {
                EnzThemeMode::Dark
            } else {
                EnzThemeMode::Light
            }));
        }
        let store = messages_core::fixture::sample_store();
        let storage_path = crate::persistence::state_path();
        let selected = store.conversations.first().map(|room| room.key.clone());
        let app = Self {
            themes,
            window_size: iced::Size::new(1280.0, 800.0),
            window_focused: true,
            settings_page: "accounts".into(),
            devices: Default::default(),
            calls: Default::default(),
            settings_drilled_in: false,
            popup: None,
            shared_section: SharedSection::default(),
            store,
            demo: true,
            nav: NavSplitState::new(),
            sidebar_scroll_offset: 0.0,
            selected,
            show_detail: false,
            filter: Filter::All,
            account_filter: None,
            selected_messages: HashSet::new(),
            collapsed_rooms: HashSet::new(),
            composer_height: 94.0,
            search: String::new(),
            search_hit: None,
            editor: text_editor::Content::new(),
            editing: None,
            dialog: None,
            dialogs: DialogStack::new(),
            confirmation: enzui_iced_core::confirm::ConfirmState::new(None),
            instance: enzui_iced_widgets::single_instance::SingleInstance::standalone(),
            jobs: jobs::Registry::default(),
            send_admissions: HashMap::new(),
            stop_control: None,
            stop_mode: None,
            boot_nonce: Uuid::new_v4(),
            overflow_open: false,
            sidebar_overflow_open: false,
            notice: None,
            notice_toasts: enzui_iced_widgets::toast_host::ToastHost::new()
                .id_prefix("messages-toast"),
            notice_toast: None,
            notice_expiry: Default::default(),
            context_menu: Default::default(),
            context_showing: Uuid::new_v4(),
            account_form: AccountForm::default(),
            avatars: avatars::Avatars::default(),
            platform_model: accounts::platform_model(),
            platform_menu: enzui_iced_core::menu_model::MenuState::closed(),
            pending_service: None,
            service_task: None,
            services_stopping: false,
            managed_login_checks: HashMap::new(),
            managed_hosts: HashMap::new(),
            new_account: None,
            new_recipient: String::new(),
            new_busy: false,
            new_request: None,
            thread_name: String::new(),
            login: None,
            registry: Registry::default(),
            connections: Vec::new(),
            adapters: HashMap::new(),
            sessions: HashMap::new(),
            history_busy: HashSet::new(),
            history_errors: HashMap::new(),
            scroll_positions: HashMap::new(),
            cursors: HashMap::new(),
            syncing: HashSet::new(),
            sync_retry: HashMap::new(),
            loading: true,
            persistence_blocked: false,
            revision: 0,
            saved_revision: 0,
            saving: false,
            dispatch_after_save: Vec::new(),
            dark_override,
            theme_writer: theme_preferences::Writer::from_identity(),
            closing: None,
            chooser: enzui_iced_widgets::file_chooser::ChooserSession::new().with_fallback(),
            chooser_start: chooser::Controller::default(),
            drop_admissions: HashMap::new(),
            typing: HashMap::new(),
            typing_sent: HashMap::new(),
            read_sent: HashMap::new(),
            preferences: crate::persistence::Preferences::default(),
            shortcuts: enzui_iced_core::EditableShortcutCatalog::new(shortcuts::catalog()),
            shortcut_armed: None,
            shortcut_feedback: None,
            bubble_picker_open: None,
            emoji_query: String::new(),
            emoji_category: Default::default(),
            emoji_recents: Default::default(),
            emoji_generation: Uuid::new_v4(),
            notifications: None,
            notification_retry: std::time::Instant::now(),
            notification_connecting: true,
            notification_pending: HashMap::new(),
            notification_bindings: HashMap::new(),
            notification_activations: HashMap::new(),
            notification_seen: HashMap::new(),
            notification_since: HashMap::new(),
            file_busy: HashSet::new(),
            file_reclaims: Vec::new(),
            file_tasks: HashMap::new(),
            file_operations: HashSet::new(),
            preview_pool: previews::worker_pool(),
            previews: HashMap::new(),
            visible_previews: HashSet::new(),
            preview_tasks: HashMap::new(),
            storage_path,
            credentials: None,
            credential_keys: Default::default(),
            credential_cleanup: Vec::new(),
            account_purges: Vec::new(),
            erase_after: Vec::new(),
            erase_hosts: HashMap::new(),
            erasing: HashSet::new(),
            credential_pending: Vec::new(),
            credential_busy: HashSet::new(),
            credential_failed: HashSet::new(),
            credential_restoring: HashSet::new(),
            credential_notice: None,
        };
        let initial = Task::batch([
            boot.map(|event| Message::Theme(Box::new(event))),
            app.track_job(
                jobs::Kind::Loading,
                async {
                    let result = crate::persistence::load_workspace().await;
                    let photos = if let Ok(Some(snapshot)) = &result {
                        crate::platform::avatar_cache::load(
                            crate::persistence::state_path(),
                            &snapshot.store,
                        )
                        .await
                    } else {
                        Vec::new()
                    };
                    (result, photos)
                },
                |(result, photos)| Message::WorkspaceLoaded(result, photos),
            ),
            app.track_job(
                jobs::Kind::Notifications,
                crate::platform::notifications::Client::connect(),
                Message::NotificationsReady,
            ),
            app.track_job(
                jobs::Kind::Credentials,
                crate::platform::credentials::Credentials::connect(),
                Message::CredentialsReady,
            ),
        ]);
        (app, initial)
    }

    pub fn theme(&self) -> iced::Theme {
        self.themes.iced_theme()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let tick = if self.adapters.is_empty()
            && self.typing.is_empty()
            && !self
                .store
                .conversations
                .iter()
                .any(|room| room.muted_until.is_some())
            && self.revision == self.saved_revision
        {
            Subscription::none()
        } else {
            iced::time::every(std::time::Duration::from_secs(2)).map(|_| Message::Tick)
        };
        Subscription::batch([
            self.notifications
                .as_ref()
                .map_or_else(Subscription::none, |client| client.subscription()),
            self.themes
                .subscription()
                .map(|event| Message::Theme(Box::new(event))),
            self.instance.subscription(|_| Message::Launched),
            iced::event::listen_with(|event, status, _| match event {
                iced::Event::Window(iced::window::Event::Focused) => {
                    Some(Message::WindowFocused(true))
                }
                iced::Event::Window(iced::window::Event::Unfocused) => {
                    Some(Message::WindowFocused(false))
                }
                iced::Event::Keyboard(event) => Some(Message::Shortcut(
                    event,
                    status == iced::event::Status::Captured,
                )),
                _ => None,
            }),
            self.chooser.subscription().map(|_| Message::ChooserWake),
            enzui_iced_widgets::file_chooser::completion_signal_subscription(0, self.jobs.signal())
                .map(|_| Message::WorkChanged),
            if self.devices.camera_on
                && self.dialog == Some(Dialog::Settings)
                && self.settings_page == "devices"
            {
                iced::time::every(std::time::Duration::from_millis(500))
                    .map(|_| Message::Devices(devices::Event::FrameTick))
            } else {
                Subscription::none()
            },
            if self
                .calls
                .active
                .as_ref()
                .is_some_and(|call| !call.snapshot.phase.finished())
            {
                iced::time::every(std::time::Duration::from_millis(100))
                    .map(|_| Message::Call(calls::Event::Tick))
            } else {
                Subscription::none()
            },
            tick,
            self.notice_expiry
                .armed()
                .map_or_else(Subscription::none, |(deadline, wait)| {
                    iced::time::every(wait).with(deadline).map(|(_, now)| {
                        Message::NoticeToast(enzui_iced_widgets::toast_host::ToastEvent::Tick(now))
                    })
                }),
            iced::window::close_requests().map(Message::CloseRequested),
            iced::window::resize_events().map(|(_, size)| Message::WindowResized(size)),
        ])
    }

    pub fn visible_rooms(&self) -> Vec<&messages_core::Conversation> {
        self.store
            .visible_rooms(
                &self.search,
                None,
                self.filter == Filter::Unread,
                self.filter == Filter::Archived,
            )
            .into_iter()
            .filter(|room| self.account_filter.is_none_or(|id| room.key.account == id))
            .collect()
    }

    pub fn current_room(&self) -> Option<&messages_core::Conversation> {
        self.selected.as_ref().and_then(|key| self.store.room(key))
    }

    pub fn changed(&mut self) {
        self.revision = self.revision.saturating_add(1);
    }

    fn save(&mut self) -> Task<Message> {
        if self.saving
            || self.loading
            || self.persistence_blocked
            || self.revision == self.saved_revision
        {
            return Task::none();
        }
        self.saving = true;
        for config in &mut self.connections {
            if config.protocol.as_str() == "signal" {
                config.rooms = self
                    .store
                    .conversations
                    .iter()
                    .filter(|room| room.key.account == config.account)
                    .map(|room| room.key.room.clone())
                    .collect();
            }
        }
        let revision = self.revision;
        let snapshot = crate::persistence::Snapshot {
            version: 1,
            preferences: self.preferences.clone(),
            store: self.store.clone(),
            demo: self.demo,
            selected: self.selected.clone(),
            dark: None,
            connections: self.connections.clone(),
            credential_keys: self.credential_keys.clone(),
            credential_cleanup: self.credential_cleanup.clone(),
            account_purges: self.account_purges.clone(),
        };
        self.track_job(
            jobs::Kind::Persistence,
            crate::persistence::save(self.snapshot_path(), snapshot),
            move |result| Message::Saved(revision, result),
        )
    }

    pub(super) fn snapshot_path(&self) -> std::path::PathBuf {
        if self.demo {
            self.storage_path.with_file_name("sample-state.json")
        } else {
            self.storage_path.clone()
        }
    }

    pub(super) fn request_close(&mut self, id: iced::window::Id) -> Task<Message> {
        if self.calls.active.is_some() {
            self.closing = Some(id);
            self.retire_call();
            return Task::none();
        }
        self.devices.stop();
        self.chooser_start.cancel();
        let _ = self.chooser.parent_closed(id);
        self.cancel_service();
        self.chooser
            .cancel(enzui_iced_widgets::file_chooser::CancelReason::Application);
        self.new_request = None;
        self.new_busy = false;
        self.login = None;
        self.closing = Some(id);
        if self.loading || self.services_stopping {
            return Task::none();
        }
        if self.revision == self.saved_revision
            && !self.saving
            && !self.theme_writer.active
            && self.theme_writer.pending.is_none()
            && self.file_busy.is_empty()
            && self.file_operations.is_empty()
            && self.credential_busy.is_empty()
            && self.credential_pending.is_empty()
        {
            if self.managed_hosts.is_empty() && self.erase_hosts.is_empty() {
                return iced::window::close(id);
            }
            self.services_stopping = true;
            // Removed accounts' helpers stop here too; their tombstones keep the
            // erasure owed to the next start.
            let hosts: Vec<_> = std::mem::take(&mut self.managed_hosts)
                .into_values()
                .chain(std::mem::take(&mut self.erase_hosts).into_values())
                .collect();
            return self.track_job(
                jobs::Kind::Shutdown,
                async move {
                    iced::futures::future::join_all(hosts.iter().map(|host| host.stop())).await;
                },
                move |_| Message::ServicesStopped(id),
            );
        }
        self.save()
    }
}
