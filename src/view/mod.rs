mod account_login;
mod bubble_contrast;
mod calls;
mod context;
mod conversation;
mod devices;
mod dialogs;
mod new_conversation;
mod picker;
mod settings;
mod shared;
mod sidebar;
pub(crate) mod transcript;

use enzui_iced_core::localization::text;
use enzui_iced_widgets::app_shell::{
    ShellLayout, ShellTokens, SidebarHeader, enz_app_shell_with_geometry,
};
use enzui_iced_widgets::avatar::{AvatarTokens, enz_avatar};
use enzui_iced_widgets::button::{ButtonKind, ButtonSize, enz_button};
use enzui_iced_widgets::text::{TextSize, TextTone, enz_text};
use enzui_iced_widgets::title_bar::enz_title_bar;
use enzui_iced_widgets::toolbar_icon::enz_toolbar_icon;
use iced::advanced::widget::Id as WidgetId;
use iced::widget::{Space, container, row};
use iced::{Element, Length};

use crate::app::{App, Dialog, Message};

pub fn root(app: &App) -> Element<'_, Message> {
    let _theme_scope =
        enzui_iced_theme::text_defaults::TextDefaultsScope::enter(app.themes.theme());
    let compact = app.window_size.width < 760.0;
    let shell = enz_app_shell_with_geometry(move |geometry| {
        themed(app, || {
            let pane = if app.loading {
                container(
                    enzui_iced_widgets::status_page::enz_status_page("Loading conversations…")
                        .icon("messages-square")
                        .into_element(),
                )
                .center(Length::Fill)
                .into()
            } else if !compact || app.show_detail || app.store.accounts.is_empty() {
                conversation::pane(app, compact, geometry)
            } else {
                container(sidebar::conversation_list(
                    app,
                    Length::Fill,
                    app.nav.overlay_open,
                    geometry.content_width,
                ))
                .padding(iced::Padding::ZERO.top(geometry.content_inset[0]))
                .height(Length::Fill)
                .into()
            };
            container(pane)
                .padding(iced::Padding::ZERO)
                .height(Length::Fill)
                .into()
        })
    })
    .layout(if compact {
        ShellLayout::FloatingTitlebar
    } else {
        ShellLayout::FloatingTitlebarSidebar
    })
    .tokens(ShellTokens {
        sidebar_width: 272.0,
        sidebar_header: SidebarHeader::Inside,
        edge_inset: Some(app.themes.theme().spacing.space2),
        ..ShellTokens::inset_panes()
            .gap(app.themes.theme().spacing.space3)
            .radius(app.themes.theme().window_radius)
    })
    .breakpoints(app.themes.theme().breakpoints)
    .sidebar_titlebar(|| {
        themed(app, || {
            enz_title_bar()
                .title(text("messages-messages"))
                .actions(app_actions(), Message::ToggleSidebarOverflow)
                .action_menu_id_prefix("messages-sidebar-actions")
                .action_overflow_trigger_id("messages-sidebar-actions-overflow")
                .overflow_open(app.sidebar_overflow_open)
                .action_dismiss(Message::DismissSidebarOverflow)
                .id("messages-sidebar-title")
                .into_element()
        })
    })
    .sidebar_with_geometry(|geometry| {
        themed(app, || {
            sidebar::conversation_list(
                app,
                Length::Fill,
                geometry.sidebar.is_overlay(),
                geometry.sidebar_width,
            )
        })
    })
    .titlebar(|| themed(app, || titlebar(app)))
    .titlebar_scrim(true)
    .on_window_gesture(Message::Window)
    .sidebar_toggle_tooltip(false)
    .state(app.nav)
    .sidebar_label(text("messages-conversations"))
    .on_sidebar_state(Message::Sidebar)
    .into_element();
    let shell = context::host(app, shell);
    let host = app.dialogs.view_with(
        shell,
        |dialog| dialogs::dialog(app, dialog),
        Message::Modal,
        |dialog, host| match dialog {
            // Settings is a sheet from the bottom edge in a small window.
            Dialog::Settings => host.placement(
                match enzui_iced_widgets::settings_dialog::enz_settings_presentation_for(
                    app.window_size,
                ) {
                    enzui_iced_widgets::settings_dialog::EnzSettingsPresentation::Sheet => {
                        enzui_iced_widgets::modal_host::SurfacePlacement::BottomEdge
                    }
                    _ => enzui_iced_widgets::modal_host::SurfacePlacement::Centred,
                },
            ),
            _ => host,
        },
    );
    let host = app.chooser.view(host, Message::ChooserEvent);
    let root = enzui_iced_widgets::toast_host::enz_toast_layer(
        host,
        corner(app),
        // Above the composer while a conversation is open, so a notice never
        // covers the send button.
        enzui_iced_widgets::toast_host::ToastOverlayPlacement::default().bottom_inset(
            if app.selected.is_some() {
                app.composer_height
            } else {
                0.0
            },
        ),
    );
    // File lists outside the composer must remain COPY-only and unacknowledged.
    enzui_iced_widgets::file_drop::enz_file_drop_target_with_receipt(root, |_| {
        Message::ActivationFinished
    })
    .copy_only(true)
    .highlight(false)
    .key("messages-reject-outside-composer")
    .into()
}

fn themed<'a>(app: &'a App, build: impl FnOnce() -> Element<'a, Message>) -> Element<'a, Message> {
    let _scope = enzui_iced_theme::text_defaults::TextDefaultsScope::enter(app.themes.theme());
    build()
}

fn titlebar(app: &App) -> Element<'_, Message> {
    let mut bar = enz_title_bar().tone(enzui_iced_widgets::surface::SurfaceTone::Transparent);
    if let Some(room) = app
        .current_room()
        .filter(|_| !app.loading && (app.window_size.width >= 760.0 || app.show_detail))
    {
        if app.window_size.width < 760.0 && app.show_detail {
            bar = bar.leading(
                enz_toolbar_icon("navigation.back", Message::Back)
                    .id(WidgetId::new("messages-back"))
                    .aria_label(text("messages-back")),
            );
        }
        bar = bar
            .push(
                row![
                    profile_action(
                        room_avatar(app, room, 36.0),
                        "Conversation profile".into(),
                        "messages-title-avatar"
                    ),
                    iced::widget::column![
                        profile_action(
                            row![
                                enz_text(&room.title)
                                    .size(TextSize::Body)
                                    .single_line_elided(),
                                encryption_lock(room.encryption)
                            ]
                            .spacing(6)
                            .align_y(iced::Alignment::Center)
                            .into(),
                            format!("Conversation details: {}", room.title),
                            "messages-title-name"
                        ),
                        hint(&room.subtitle),
                    ]
                    .width(Length::Shrink),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center)
                .width(Length::Shrink),
            )
            .push(Space::new().width(Length::Fill))
            .fluid_content(70.0)
            .actions(room_actions(app, room), Message::ToggleOverflow);
    } else {
        bar = bar.title(text("messages-messages"));
        if app.window_size.width < 760.0 {
            bar = bar.actions(app_actions(), Message::ToggleOverflow);
        }
    }
    bar = bar
        .action_menu_id_prefix("messages-actions")
        .action_overflow_trigger_id("messages-actions-overflow")
        .overflow_open(app.overflow_open)
        .action_dismiss(Message::DismissOverflow);
    bar.id("messages-window").into_element()
}

fn action<'a>(
    id: &'static str,
    label: &'static str,
    icon: &'static str,
    message: Message,
) -> enzui_iced_widgets::action_bar::ActionItem<'a, Message> {
    enzui_iced_widgets::action_bar::ActionItem::icon(id, icon, text(label), message)
}

fn app_actions<'a>() -> Vec<enzui_iced_widgets::action_bar::ActionItem<'a, Message>> {
    vec![
        action(
            "messages-new-conversation",
            "messages-new-conversation",
            "square-pen",
            Message::Open(Dialog::NewConversation),
        ),
        action(
            "messages-settings",
            "messages-settings",
            "settings",
            Message::Open(Dialog::Settings),
        ),
    ]
}

fn room_actions<'a>(
    app: &'a App,
    room: &'a messages_core::Conversation,
) -> Vec<enzui_iced_widgets::action_bar::ActionItem<'a, Message>> {
    use enzui_iced_core::action_overflow::{Descriptor, Presentation};
    use enzui_iced_widgets::action_bar::ActionItem;
    let mut actions = vec![
        ActionItem::with_inline(
            Descriptor::new("messages-mute")
                .pinned()
                .with_presentation(Presentation::Compact),
            enzui_iced_core::Action::new("messages-mute", Message::Mute)
                .with_label("Notifications")
                .with_icon(enzui_iced_core::Icon::named("bell")),
            move |_, _| picker::mute_button(app, room, false),
        ),
        action(
            "messages-details",
            "messages-details",
            "info",
            Message::Open(Dialog::Details),
        ),
    ];
    if app.calls.active.is_some() {
        actions.push(ActionItem::icon(
            "messages-current-call",
            "phone",
            "Current call",
            Message::Open(Dialog::Call),
        ));
    } else {
        for (id, kind, icon, label) in [
            (
                "messages-voice-call",
                messages_protocols::calls::CallKind::Voice,
                "phone",
                "Voice call",
            ),
            (
                "messages-video-call",
                messages_protocols::calls::CallKind::Video,
                "video",
                "Video call",
            ),
        ] {
            if app.demo
                || !app
                    .store
                    .capabilities(&room.key)
                    .supports(kind.capability())
            {
                continue;
            }
            actions.push(ActionItem::command(
                Descriptor::new(id).with_presentation(Presentation::Compact),
                enzui_iced_core::Action::new(
                    id,
                    Message::Call(crate::app::calls::Event::Start(
                        room.key.clone(),
                        kind,
                        app.session(room.key.account),
                    )),
                )
                .with_label(label)
                .with_icon(enzui_iced_core::Icon::named(icon))
                .with_enabled(
                    !app.demo
                        && app
                            .store
                            .capabilities(&room.key)
                            .supports(kind.capability()),
                ),
            ));
        }
    }
    actions
}

fn avatar<'a>(app: &App, name: &str, size: f32) -> Element<'a, Message> {
    enz_avatar(name)
        .avatar_size(size)
        .tokens(avatar_tokens(app, name))
        .into_element()
}

fn room_avatar<'a>(
    app: &App,
    room: &messages_core::Conversation,
    size: f32,
) -> Element<'a, Message> {
    enz_avatar(&room.title)
        .avatar_size(size)
        .tokens(avatar_tokens(app, &room.title))
        .media(
            app.avatar_image(&room.key)
                .map(enzui_iced_widgets::avatar::AvatarMedia::Image)
                .unwrap_or_default(),
        )
        .into_element()
}

fn avatar_tokens(app: &App, name: &str) -> AvatarTokens {
    let palette = &app.themes.theme().palette;
    let swatches = [
        iced::Color::from_rgb8(126, 104, 166),
        iced::Color::from_rgb8(77, 137, 150),
        iced::Color::from_rgb8(171, 121, 91),
        iced::Color::from_rgb8(109, 142, 100),
        iced::Color::from_rgb8(175, 103, 132),
    ];
    let index = name.bytes().fold(0usize, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(byte as usize)
    }) % swatches.len();
    let pair = palette.filled_color(swatches[index]);
    AvatarTokens {
        background: pair.color,
        foreground: pair.text,
        ..AvatarTokens::from_palette(palette)
    }
}

fn hint<'a>(text: impl Into<String>) -> Element<'a, Message> {
    enz_text(text.into())
        .size(TextSize::Caption)
        .tone(TextTone::Muted)
        .into_element()
}

// The bottom-right corner: a warning stays as a notice until it is closed;
// transient progress is a toast that expires.
fn corner(app: &App) -> Option<Element<'_, Message>> {
    let notice = notice(app);
    let toasts =
        (!app.notice_toasts.is_empty()).then(|| app.notice_toasts.view().map(Message::NoticeToast));
    match (notice, toasts) {
        (None, None) => None,
        (notice, toasts) => Some(
            iced::widget::column(notice.into_iter().chain(toasts))
                .spacing(app.themes.theme().spacing.xs())
                .align_x(iced::Alignment::End)
                .into(),
        ),
    }
}

fn notice(app: &App) -> Option<Element<'_, Message>> {
    app.notice
        .as_ref()
        .filter(|notice| !notice.transient())
        .map(|notice| {
            enzui_iced_widgets::notice_stack::enz_notice_stack(vec![
                enzui_iced_widgets::notice_stack::EnzNotice::new(
                    "messages-notice",
                    &notice.text,
                    notice.tone(),
                )
                .on_dismiss(Message::DismissNotice),
            ])
            .width(Length::Fill.max(360.0))
            .into_element()
        })
}

fn encryption_lock(encryption: messages_core::Encryption) -> Element<'static, Message> {
    if encryption == messages_core::Encryption::EndToEnd {
        enzui_iced_widgets::tooltip::enz_tooltip(
            enzui_iced_widgets::icon::enz_icon("lock-keyhole")
                .size(12.0)
                .into_element(),
            "This chat is end-to-end encrypted",
            iced::widget::tooltip::Position::Bottom,
        )
    } else {
        Space::new().into()
    }
}

fn profile_action<'a>(
    content: Element<'a, Message>,
    label: String,
    id: &'static str,
) -> Element<'a, Message> {
    use enzui_iced_core::semantics::{Role, Semantics};
    use enzui_iced_widgets::focus_button::{FocusStatus, focus_button};
    focus_button(content, Message::Open(Dialog::Details))
        .id(WidgetId::new(id))
        .semantics(Semantics::new(Role::Button).label(label))
        .padding(0)
        .width(Length::Shrink)
        .inset_focus_ring(true)
        .style_focus(|theme: &iced::Theme, status| iced::widget::button::Style {
            text_color: theme.palette().background.base.text,
            border: if status == FocusStatus::Focused {
                iced::Border {
                    width: 2.0,
                    color: theme.palette().primary.base.color,
                    radius: 4.0.into(),
                }
            } else {
                iced::Border::default()
            },
            ..Default::default()
        })
        .into()
}
