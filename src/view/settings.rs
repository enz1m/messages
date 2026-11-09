use super::*;
use crate::app::customization::{BubbleSide, Preference};
use enzui_iced_widgets::combo_box::enz_combo_box;
use enzui_iced_widgets::preferences_row::enz_preferences_row;
use iced::widget::column;
use messages_core::AccountId;

pub(super) fn view(app: &App) -> Element<'_, Message> {
    let mut accounts =
        column![enz_text(text("messages-accounts")).size(TextSize::Headline)].spacing(12);
    for account in &app.store.accounts {
        accounts = accounts.push(
            enzui_iced_widgets::list_row_surface::enz_list_row_surface(
                row![
                    avatar(app, &account.label, 32.0),
                    column![
                        enz_text(&account.label).size(TextSize::Body),
                        hint(&account.user_id)
                    ]
                    .spacing(2)
                    .width(Length::Fill),
                    hint(account.connection.label()),
                    enzui_iced_widgets::icon::enz_icon("navigation.forward")
                        .size(16.0)
                        .into_element()
                ]
                .spacing(10)
                .align_y(iced::Alignment::Center),
                Message::Open(Dialog::AccountSettings(account.id)),
            )
            .row_height(52.0)
            .content_padding(8.0)
            .label(&account.label)
            .id_prefix(format!("messages-settings-account-{}", account.id)),
        );
    }
    accounts = accounts.push(
        enz_button(
            text("messages-add-account"),
            Message::Open(Dialog::ConnectAccount),
        )
        .id(WidgetId::new("messages-settings-add"))
        .kind(ButtonKind::Secondary)
        .icon("plus")
        .size(ButtonSize::Sm),
    );
    if let Some(notice) = &app.credential_notice {
        accounts = accounts.push(hint(notice));
    }
    if !app.credential_failed.is_empty() {
        accounts = accounts.push(
            enz_button("Retry keyring cleanup", Message::RetryCredentials)
                .id(WidgetId::new("messages-settings-keyring-retry"))
                .kind(ButtonKind::Secondary),
        );
    }
    let theme = match app.dark_override {
        Some(true) => "Dark",
        Some(false) => "Light",
        None => "Follow system",
    };
    let size = format!("{} px", app.preferences.text_size);
    let rounding = if app.preferences.bubble_radius <= 6.0 {
        "Subtle"
    } else if app.preferences.bubble_radius >= 16.0 {
        "Soft"
    } else {
        "Rounded"
    };
    let appearance = column![
        enz_text(text("messages-appearance")).size(TextSize::Headline),
        enz_combo_box(
            text("messages-theme"),
            vec!["Follow system".into(), "Light".into(), "Dark".into()],
            Some(theme.into()),
            |value| Message::SetTheme(match value.as_str() {
                "Dark" => Some(true),
                "Light" => Some(false),
                _ => None,
            })
        ),
        enz_combo_box(
            text("messages-text-size"),
            (11..=22).map(|n| format!("{n} px")).collect(),
            Some(size),
            |value| Message::Preference(Preference::TextSize(
                value.trim_end_matches(" px").parse().unwrap_or(13)
            ))
        ),
        bubble_colors(app, BubbleSide::Sent),
        bubble_colors(app, BubbleSide::Received),
        enz_combo_box(
            text("messages-corners"),
            vec!["Subtle".into(), "Rounded".into(), "Soft".into()],
            Some(rounding.into()),
            |value| Message::Preference(Preference::BubbleRadius(match value.as_str() {
                "Subtle" => 6.0,
                "Soft" => 18.0,
                _ => 10.0,
            }))
        ),
        switch_row(
            app.preferences.group_messages,
            text("messages-group-times"),
            |value| Message::Preference(Preference::GroupMessages(value))
        ),
        bubble_preview(app),
    ]
    .spacing(14);
    let privacy = column![
        enz_text(text("messages-privacy")).size(TextSize::Headline),
        hint("Read receipts and typing indicators are configured separately for each account in Accounts."),
    ].spacing(16);
    let mut notifications = column![
        enz_text("Notifications").size(TextSize::Headline),
        switch_row(app.preferences.notifications, "Desktop notifications", |v|Message::Preference(Preference::Notifications(v))),
        switch_row(app.preferences.notification_preview, "Show message previews", |v|Message::Preference(Preference::NotificationPreview(v))),
        hint("Notifications appear for unread incoming messages. Muted conversations stay quiet. System permissions and Do Not Disturb also apply."),
        hint(if app.notifications.as_ref().is_some_and(|n|n.ready()){"Desktop notification service connected"}else{"Desktop notification service unavailable or connecting"}),
        enz_button("Send test notification",Message::TestNotification).id(WidgetId::new("messages-test-notification")).enabled(app.preferences.notifications && app.notifications.as_ref().is_some_and(|n|n.ready())),
        if app.notifications.is_none() { enz_button("Retry notification service", Message::RetryNotifications).id(WidgetId::new("messages-retry-notifications")).enabled(!app.notification_connecting).into_element() } else { Space::new().into() },
        enz_text("Accounts").size(TextSize::Caption),
    ].spacing(14);
    for account in &app.store.accounts {
        let id = account.id;
        notifications = notifications.push(
            enz_preferences_row(account.label.clone())
                .switch(app.preferences.account(id).notifications, move |v| {
                    Message::Preference(Preference::AccountNotifications(id, v))
                })
                .label_min_width(100.0)
                .id_prefix(format!("messages-notifications-account-{id}"))
                .into_element(),
        );
    }
    let settings = enzui_iced_widgets::settings_dialog::enz_settings_dialog(true, app.window_size)
        .title(text("messages-settings"))
        .page(
            enzui_iced_widgets::settings_dialog::SettingsPage::new(
                "accounts",
                text("messages-accounts"),
            )
            .icon("users"),
            accounts,
        )
        .page(
            enzui_iced_widgets::settings_dialog::SettingsPage::new(
                "appearance",
                text("messages-appearance"),
            )
            .icon("palette"),
            appearance,
        )
        .page(
            enzui_iced_widgets::settings_dialog::SettingsPage::new(
                "notifications",
                "Notifications",
            )
            .icon("bell"),
            notifications,
        )
        .page(
            enzui_iced_widgets::settings_dialog::SettingsPage::new("privacy", "Privacy")
                .icon("shield"),
            privacy,
        )
        .page(
            enzui_iced_widgets::settings_dialog::SettingsPage::new(
                "shortcuts",
                "Keyboard shortcuts",
            )
            .icon("keyboard"),
            enzui_iced_widgets::editable_shortcuts::enz_editable_shortcuts(
                &app.shortcuts,
                app.shortcut_armed.as_deref(),
                Message::ShortcutSettings,
            )
            .feedback(
                app.shortcut_feedback
                    .as_ref()
                    .map(|(id, text)| (id.as_str(), text.as_str())),
            )
            .into_element(),
        )
        .page(
            enzui_iced_widgets::settings_dialog::SettingsPage::new("devices", "Devices")
                .icon("camera"),
            super::devices::view(app),
        )
        .selected(&app.settings_page)
        .stacked_navigation(app.settings_drilled_in, Message::SettingsOverview)
        .on_select(Message::SettingsPage)
        .on_done(Message::CloseDialog);
    // Reset-all sits in the footer whenever the shortcuts page is shown: as a
    // tab, or opened from the narrow window's page list.
    let shortcuts_shown = app.settings_page == "shortcuts"
        && (app.settings_drilled_in
            || settings.resolved_nav()
                != enzui_iced_widgets::settings_dialog::SettingsNav::Stacked);
    let settings = settings.actions(shortcuts_shown.then(|| {
        enzui_iced_widgets::editable_shortcuts::editable_shortcuts_reset_all_action(|| {
            Message::ShortcutSettings(
                enzui_iced_widgets::editable_shortcuts::EditableShortcutsEvent::ResetAll,
            )
        })
    }));
    // A centred dialog, or a sheet from the bottom edge in a small window
    // (hosted at the matching placement in `view::mod`).
    settings.into_presented_surface(Some(Box::new(|| Message::CloseDialog)))
}

fn switch_row<'a>(
    value: bool,
    label: impl Into<std::borrow::Cow<'a, str>>,
    on_toggle: impl Fn(bool) -> Message + 'a,
) -> Element<'a, Message> {
    enz_preferences_row(label)
        .switch(value, on_toggle)
        .label_min_width(100.0)
        .id_prefix("messages-preference")
        .into_element()
}

fn bubble_preview(app: &App) -> Element<'_, Message> {
    use enzui_iced_widgets::chat_bubble::{ChatRole, enz_chat_bubble};
    column![
        enz_text("Preview").size(TextSize::Body),
        enz_chat_bubble("Got it. See you soon.", ChatRole::Assistant)
            .tokens(super::conversation::bubble_tokens(app))
            .into_element(),
        enz_chat_bubble("A little more you.", ChatRole::User)
            .tokens(super::conversation::bubble_tokens(app))
            .into_element(),
    ]
    .spacing(8)
    .into()
}

pub(super) fn account(app: &App, id: AccountId) -> Element<'_, Message> {
    let Some(account) = app.store.accounts.iter().find(|a| a.id == id) else {
        return hint("This account has been removed.");
    };
    let prefs = app.preferences.account(id);
    let mut body = column![
        row![
            avatar(app, &account.label, 40.0),
            column![
                enz_text(&account.label).size(TextSize::Headline),
                hint(&account.user_id),
                hint(account.connection.label())
            ]
            .spacing(3)
        ]
        .spacing(12)
        .align_y(iced::Alignment::Center),
        switch_row(
            prefs.notifications,
            "Notifications for this account",
            move |value| Message::Preference(Preference::AccountNotifications(id, value))
        ),
    ]
    .spacing(16);
    if account
        .capabilities
        .supports(messages_core::Capability::ReadReceipts)
    {
        body = body.push(switch_row(
            prefs.read_receipts,
            "Send read receipts",
            move |value| Message::Preference(Preference::AccountRead(id, value)),
        ));
    }
    if account
        .capabilities
        .supports(messages_core::Capability::Typing)
    {
        body = body.push(switch_row(
            prefs.typing_indicators,
            "Share typing indicators",
            move |value| Message::Preference(Preference::AccountTyping(id, value)),
        ));
    }
    if app
        .adapters
        .get(&id)
        .is_some_and(|adapter| adapter.supports_encryption_recovery())
    {
        body = body.push(
            enz_button("Restore encryption keys", Message::MatrixRecovery(id))
                .id(WidgetId::from(format!("messages-account-{id}-keys")))
                .kind(ButtonKind::Secondary)
                .icon("shield"),
        );
    }
    body.push(
        row![
            enz_button("Reconnect", Message::Reconnect(id))
                .id(WidgetId::from(format!("messages-account-{id}-reconnect")))
                .kind(ButtonKind::Secondary)
                .size(ButtonSize::Sm),
            enz_button("Sign in", Message::LoginLoad(id))
                .id(WidgetId::from(format!("messages-account-{id}-login")))
                .kind(ButtonKind::Quiet)
                .size(ButtonSize::Sm)
                .enabled(app.adapters.contains_key(&id)),
            Space::new().width(Length::Fill),
            enz_toolbar_icon("trash-2", Message::Open(Dialog::RemoveAccount(id)))
                .id(WidgetId::from(format!("messages-account-{id}-remove")))
                .aria_label("Remove account")
                .tooltip("Remove account")
        ]
        .spacing(8),
    )
    .push(
        enz_button("Back to settings", Message::Open(Dialog::Settings))
            .id(WidgetId::from(format!("messages-account-{id}-back")))
            .kind(ButtonKind::Quiet)
            .icon("navigation.back")
            .size(ButtonSize::Sm),
    )
    .into()
}

fn bubble_colors(app: &App, side: BubbleSide) -> Element<'_, Message> {
    use enzui_iced_widgets::color_picker::{Hsva, enz_color_picker};
    let (label, id, selected, default) = match side {
        BubbleSide::Sent => (
            "Sent bubble color",
            "sent",
            &app.preferences.bubble_color,
            "System accent",
        ),
        BubbleSide::Received => (
            "Received bubble color",
            "received",
            &app.preferences.received_bubble_color,
            "System surface",
        ),
    };
    let preference = move |color| {
        Message::Preference(match side {
            BubbleSide::Sent => Preference::BubbleColor(color),
            BubbleSide::Received => Preference::ReceivedBubbleColor(color),
        })
    };
    let mut colors = row![].spacing(10).align_y(iced::Alignment::Center);
    for name in [default, "Ocean", "Forest", "Plum", "Neutral"] {
        colors = colors.push(
            enzui_iced_widgets::color_swatch::enz_color_swatch(
                super::conversation::bubble_fill(app, name),
                preference(name.into()),
            )
            .size(30.0)
            .radius(15.0)
            .aria_label(format!("{label}: {name}"))
            .id_prefix(format!("messages-bubble-{id}-{name}"))
            .selected(selected == name)
            .into_element(),
        );
    }
    let mut spectrum = iced::gradient::Linear::new(iced::Radians(std::f32::consts::FRAC_PI_4));
    for (position, color) in [
        (0.0, iced::Color::from_rgb8(216, 79, 108)),
        (0.25, iced::Color::from_rgb8(229, 165, 61)),
        (0.5, iced::Color::from_rgb8(64, 165, 137)),
        (0.75, iced::Color::from_rgb8(74, 123, 205)),
        (1.0, iced::Color::from_rgb8(151, 79, 177)),
    ] {
        spectrum = spectrum.add_stop(position, color);
    }
    let custom_label = format!("Custom {}", label.to_lowercase());
    colors = colors.push(
        enzui_iced_widgets::focus_button::focus_button(
            container(
                enzui_iced_widgets::icon::enz_icon("pipette")
                    .size(15.0)
                    .tint(iced::Color::WHITE)
                    .into_element(),
            )
            .center(30.0),
            Message::ToggleBubblePicker(side),
        )
        .id(WidgetId::from(format!("messages-custom-bubble-{id}")))
        .width(30.0)
        .height(30.0)
        .padding(0)
        .content_align_x(iced::Alignment::Center)
        .content_align_y(iced::Alignment::Center)
        .semantics(
            enzui_iced_core::semantics::Semantics::new(enzui_iced_core::semantics::Role::Button)
                .label(custom_label),
        )
        .style_focus(move |theme: &iced::Theme, status| {
            let focus = matches!(
                status,
                enzui_iced_widgets::focus_button::FocusStatus::Focused
            );
            iced::widget::button::Style {
                background: Some(iced::Background::Gradient(spectrum.into())),
                border: iced::Border {
                    radius: 15.0.into(),
                    width: if focus
                        || matches!(
                            status,
                            enzui_iced_widgets::focus_button::FocusStatus::Hovered
                                | enzui_iced_widgets::focus_button::FocusStatus::Pressed
                        ) {
                        2.0
                    } else {
                        1.0
                    },
                    color: if focus {
                        theme.palette().primary.base.color
                    } else {
                        theme.palette().background.strong.color
                    },
                },
                ..Default::default()
            }
        }),
    );
    let mut section = column![enz_text(label).size(TextSize::Body), colors.wrap()].spacing(10);
    if app.bubble_picker_open == Some(side) {
        section = section.push(
            enz_color_picker(
                Hsva::from_color(super::conversation::bubble_fill(app, selected)),
                move |value| preference(value.to_hex()),
            )
            .label(format!("Custom {}", label.to_lowercase()))
            .id_prefix(format!("messages-bubble-picker-{id}"))
            .plane_height(130.0)
            .into_element(),
        );
    }
    section.into()
}
