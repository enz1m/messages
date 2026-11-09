use super::*;
use enzui_iced_widgets::{scroll_view::enz_scroll_view, text_field::enz_text_field};
use iced::widget::column;
use messages_core::{Capability, ConnectionState};

pub(super) fn view(app: &App) -> Element<'_, Message> {
    let accounts: Vec<_> = app
        .store
        .accounts
        .iter()
        .filter(|a| a.connection != ConnectionState::Demo)
        .collect();
    if accounts.is_empty() {
        return column![
            enzui_iced_widgets::icon::enz_icon("messages-square")
                .size(40.0)
                .into_element(),
            enz_text("Start with an account").size(TextSize::Headline),
            hint("Bring your conversations from Signal, WhatsApp or Matrix."),
            enz_button("Connect an account", Message::Open(Dialog::ConnectAccount))
                .id(WidgetId::new("messages-new-connect"))
                .kind(ButtonKind::Primary)
                .icon("plus"),
        ]
        .spacing(16)
        .into();
    }
    let mut account_items = Vec::new();
    let account_ids: Vec<_> = accounts.iter().map(|account| account.id).collect();
    for account in &accounts {
        let icon = app
            .registry
            .descriptor(account.protocol.as_str())
            .map(|p| p.icon)
            .unwrap_or("user");
        let label = account.label.clone();
        account_items.push(
            enzui_iced_widgets::button_group::EnzButtonGroupItem::new(
                account.id.to_string(),
                label,
            )
            .icon(icon),
        );
    }
    let account_picker =
        enzui_iced_widgets::button_group::enz_button_group(account_items, move |index| {
            Message::NewAccount(account_ids[index])
        })
        .id_prefix("messages-new-account")
        .selected(
            accounts
                .iter()
                .position(|account| app.new_account == Some(account.id)),
        )
        .kind(ButtonKind::Quiet)
        .style(enzui_iced_widgets::button_group::ButtonGroupStyle::Tonal)
        .dividers(false)
        .size(enzui_iced_widgets::text_field::TextFieldSize::Sm)
        .group_enabled(!app.new_busy)
        .into_element();
    let account = accounts
        .iter()
        .find(|account| Some(account.id) == app.new_account);
    let placeholder = match account.map(|a| a.protocol.as_str()) {
        Some("signal") => "Phone number or Signal username",
        Some("whatsapp") => "Phone number, including country code",
        Some("matrix") => "@person:server or #room:server",
        _ => "Recipient",
    };
    let can_start = account.is_some_and(|a| {
        a.capabilities.supports(Capability::StartConversation) && app.adapters.contains_key(&a.id)
    });
    let mut content = column![
        hint("Send with"),
        enz_scroll_view(account_picker)
            .axis(enzui_iced_widgets::scroll_view::Axis::Horizontal)
            .height(Length::Fixed(36.0)),
        enz_text_field("To", placeholder, &app.new_recipient, Message::NewRecipient)
            .enabled(!app.new_busy && can_start)
            .on_submit(Message::StartConversation),
        row![
            Space::new().width(Length::Fill),
            enz_button(
                if app.new_busy {
                    "Opening…"
                } else {
                    "Start conversation"
                },
                Message::StartConversation
            )
            .id(WidgetId::new("messages-new-start"))
            .kind(ButtonKind::Primary)
            .icon("arrow-right")
            .size(ButtonSize::Sm)
            .enabled(can_start && !app.new_busy && !app.new_recipient.trim().is_empty())
        ],
    ]
    .spacing(12);
    if let Some(account) = account
        && !can_start
    {
        content = content.push(hint(if app.adapters.contains_key(&account.id) { "Open an existing conversation below, or configure the channels this account can access in Settings." } else { "Reconnect this account in Settings to start a conversation." }));
    }
    let query = app.new_recipient.trim().to_lowercase();
    let rooms: Vec<_> = app
        .store
        .visible_rooms(&query, None, false, false)
        .into_iter()
        .filter(|room| app.new_account.is_none_or(|id| room.key.account == id))
        .take(8)
        .collect();
    if !rooms.is_empty() {
        content = content.push(
            enz_text(if query.is_empty() {
                "Recent conversations"
            } else {
                "Conversations"
            })
            .size(TextSize::Caption)
            .weight_rung(enzui_iced_theme::WeightRung::Semibold),
        );
        let mut recent = column![].spacing(4);
        for room in rooms {
            recent = recent.push(
                enzui_iced_widgets::list_row_surface::enz_list_row_surface(
                    row![
                        room_avatar(app, room, 40.0),
                        column![
                            enz_text(&room.title)
                                .size(TextSize::Body)
                                .single_line_elided(),
                            hint(&room.subtitle)
                        ]
                        .spacing(2)
                        .width(Length::Fill),
                        enzui_iced_widgets::icon::enz_icon("navigation.forward")
                            .size(14.0)
                            .into_element()
                    ]
                    .spacing(10)
                    .align_y(iced::Alignment::Center),
                    Message::Select(room.key.clone()),
                )
                .content_padding(10.0)
                .label(&room.title)
                .id_prefix(format!(
                    "messages-recent-{}-{}",
                    room.key.account, room.key.room
                )),
            );
        }
        content = content.push(enz_scroll_view(recent).height(Length::Shrink.max(240.0)));
    }
    content.into()
}
