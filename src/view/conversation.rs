use enzui_iced_widgets::chat_bubble::{ChatBubbleTokens, ChatRole, enz_chat_bubble};
use enzui_iced_widgets::status_page::enz_status_page;
use enzui_iced_widgets::text_area::enz_text_area;
use iced::widget::column;
use messages_core::{Capability, ChatMessage, Conversation, Delivery, RoomKind};
use messages_protocols::MessageAction;

use super::*;

pub(super) fn pane<'a>(
    app: &'a App,
    compact: bool,
    geometry: &enzui_iced_widgets::app_shell::ShellGeometry,
) -> Element<'a, Message> {
    let Some(room) = app.current_room() else {
        return container(
            enz_status_page("Your conversations, together")
                .icon("messages-square")
                .description("Connect Signal, Matrix or WhatsApp to get started.")
                .primary_action(
                    enzui_iced_core::Action::new(
                        "messages-connect",
                        Message::Open(Dialog::ConnectAccount),
                    )
                    .with_label("Add account"),
                )
                .into_element(),
        )
        .center(Length::Fill)
        .padding(iced::Padding::ZERO.top(geometry.content_inset[0]))
        .into();
    };
    let transcript = super::transcript::view(app, room, compact, geometry);
    let mut body = column![].height(Length::Fill).width(Length::Fill);
    if app.locked_history(room) {
        let mut notice = enzui_iced_widgets::banner::enz_banner("Unlock encrypted history")
            .body("Restore your encryption keys to load unavailable history. Messages this device can read appear below.")
            .icon("shield")
            .dismissible(false)
            .id_prefix(format!("messages-key-recovery-{}-{}",room.key.account,room.key.room));
        if app
            .adapters
            .get(&room.key.account)
            .is_some_and(|adapter| adapter.supports_encryption_recovery())
        {
            notice = notice
                .action_text("Restore encryption keys")
                .on_action(Message::MatrixRecovery(room.key.account));
        } else {
            notice = notice.body("Reconnect this Matrix account in Settings, then restore your encryption keys. Readable messages remain available below.");
        }
        body = body.push(
            container(notice.into_element()).padding(
                iced::Padding::ZERO
                    .top(if app.search_hit.is_some() {
                        8.0
                    } else {
                        geometry.content_inset[0] + 8.0
                    })
                    .left(if compact { 16.0 } else { 24.0 })
                    .right(if compact { 16.0 } else { 24.0 })
                    .bottom(8.0),
            ),
        );
    }
    let body = body.push(transcript);
    let body = if app.search_hit.is_some() {
        let hits = app.search_hits();
        let position = hits
            .iter()
            .position(|message| Some(&message.id) == app.search_hit.as_ref())
            .unwrap_or(0)
            + 1;
        column![
            row![
                hint(format!("{} of {} matches", position, hits.len())),
                Space::new().width(Length::Fill),
                enz_toolbar_icon("chevron-up", Message::SearchStep(-1))
                    .id(WidgetId::new("messages-search-previous"))
                    .aria_label("Previous match")
                    .tooltip("Previous match"),
                enz_toolbar_icon("chevron-down", Message::SearchStep(1))
                    .id(WidgetId::new("messages-search-next"))
                    .aria_label("Next match")
                    .tooltip("Next match"),
                enz_toolbar_icon("x", Message::Search(String::new()))
                    .id(WidgetId::new("messages-search-clear"))
                    .aria_label("Clear search")
                    .tooltip("Clear search")
            ]
            .spacing(6)
            .padding(
                iced::Padding::ZERO
                    .top(geometry.content_inset[0])
                    .left(20.0)
                    .right(20.0)
            )
            .align_y(iced::Alignment::Center),
            body
        ]
        .height(Length::Fill)
    } else {
        body
    };
    let key = room.key.clone();
    let composer = iced::widget::sensor(composer_dock(app, room))
        .key(key.clone())
        .on_resize(move |size| Message::ComposerMeasured(key.clone(), size.height));
    let jump: Element<'_, Message> = if app
        .scroll_positions
        .get(&room.key)
        .is_some_and(crate::app::history::Position::is_away_from_bottom)
    {
        iced::widget::float(
            container(enzui_iced_widgets::tooltip::enz_tooltip(
                enz_button("", Message::JumpToBottom(room.key.clone()))
                    .icon("arrow-down")
                    .icon_position(enzui_iced_widgets::button::ButtonIconPosition::Only)
                    .kind(ButtonKind::Secondary)
                    .size_tokens(enzui_iced_widgets::tokens::ButtonSizeTokens {
                        height: 40.0,
                        padding_x: 11.0,
                        icon_size: 18.0,
                        radius: 20.0,
                        ..ButtonSize::Sm.tokens()
                    })
                    .material(enzui_iced_theme::MaterialStyle::FlatFrosted)
                    .id(WidgetId::new("messages-jump-latest"))
                    .aria_label("Jump to latest messages")
                    .into_element(),
                "Jump to latest messages",
                iced::widget::tooltip::Position::Top,
            ))
            .align_right(Length::Fill)
            .align_bottom(Length::Fill)
            .padding(
                iced::Padding::ZERO
                    .right(24)
                    .bottom(app.composer_height + 8.0),
            ),
        )
        .into()
    } else {
        Space::new().into()
    };
    iced::widget::stack![
        body,
        jump,
        iced::widget::float(container(composer).align_bottom(Length::Fill)),
    ]
    .into()
}

pub(super) fn bubble<'a>(
    app: &'a App,
    room: &'a Conversation,
    message: &'a ChatMessage,
    show_stamp: bool,
) -> Element<'a, Message> {
    let role = if message.own {
        ChatRole::User
    } else {
        ChatRole::Assistant
    };
    let body = if message.deleted {
        "Message deleted"
    } else {
        &message.body
    };
    let mut stamp = message
        .timestamp
        .with_timezone(&chrono::Local)
        .format("%H:%M")
        .to_string();
    if message.edited {
        stamp.push_str(" · edited");
    }
    if message.own && !message.delivery.label().is_empty() {
        stamp.push_str(" · ");
        stamp.push_str(message.delivery.label());
        if room.kind == RoomKind::Group
            && matches!(message.delivery, Delivery::Read | Delivery::Delivered)
        {
            stamp.push_str(" by a member");
        }
    }
    let mut footer = row![if app.search_hit.as_ref() == Some(&message.id) {
        enz_text(format!("{stamp} · Search match"))
            .size(TextSize::Caption)
            .tone(TextTone::Accent)
            .into_element()
    } else {
        hint(stamp)
    }]
    .spacing(6)
    .align_y(iced::Alignment::Center);
    if let (Some(transaction), Delivery::Failed(_)) = (message.transaction, &message.delivery) {
        footer = footer.push(
            enz_button("Retry", Message::Retry(transaction))
                .id(WidgetId::from(format!(
                    "messages-retry-{}-{}-{transaction}",
                    room.key.account, room.key.room
                )))
                .size(ButtonSize::Sm)
                .kind(ButtonKind::Quiet),
        );
    }
    let mut reactions = row![].spacing(0);
    for reaction in &message.reactions {
        reactions = reactions.push(
            enz_button(
                format!("{} {}", reaction.emoji, reaction.count),
                Message::Action(
                    room.key.clone(),
                    MessageAction::React {
                        message: message.clone(),
                        emoji: reaction.emoji.clone(),
                        remove: reaction.own,
                    },
                ),
            )
            .id(WidgetId::from(format!(
                "messages-reaction-{}-{}-{}-{}",
                room.key.account, room.key.room, message.id, reaction.emoji
            )))
            .kind(ButtonKind::Quiet)
            .size_tokens(enzui_iced_widgets::tokens::ButtonSizeTokens {
                height: 22.0,
                padding_x: 3.0,
                text_size: 12.0,
                radius: enzui_iced_theme::text_defaults::shape_radius(|s| s.button),
                ..ButtonSize::Sm.tokens()
            })
            .enabled(
                app.store
                    .capabilities(&room.key)
                    .supports(Capability::React),
            ),
        );
    }
    let bubble = enz_chat_bubble(body, role)
        .tokens(ChatBubbleTokens {
            footnote_gap: if message.reactions.is_empty() {
                3.0
            } else {
                14.0
            },
            ..bubble_tokens(app)
        })
        .author(message.sender_name.as_str())
        .show_author(!message.own && room.kind != RoomKind::Direct)
        .message_id(format!(
            "{}:{}:{}",
            room.key.account, room.key.room, message.id
        ))
        .typography(app.themes.theme().typography)
        .max_width(
            if message
                .attachments
                .iter()
                .any(|file| file.mime.starts_with("image/"))
            {
                360.0
            } else {
                530.0
            },
        );
    let bubble = if show_stamp
        || message.edited
        || !message.reactions.is_empty()
        || matches!(
            message.delivery,
            Delivery::Failed(_) | Delivery::Queued | Delivery::Sending | Delivery::Uncertain
        ) {
        bubble.footnote(footer)
    } else {
        bubble
    };
    let body_ink = bubble.resolved_body_ink(&app.themes.theme().palette);
    let mut attachment_tokens = enzui_iced_widgets::toolbar_icon::ToolbarIconTokens::from_palette(
        &app.themes.theme().palette,
    );
    attachment_tokens.icon_color = body_ink;
    attachment_tokens.hover_color = body_ink;
    attachment_tokens.pressed_color = body_ink;
    attachment_tokens.disabled_color = body_ink;
    if message.own {
        attachment_tokens.hover_background = iced::Color {
            a: app.themes.theme().palette.overlay_hover().a,
            ..body_ink
        };
        attachment_tokens.pressed_background = iced::Color {
            a: app.themes.theme().palette.overlay_pressed().a,
            ..body_ink
        };
        attachment_tokens.focus_ring = body_ink;
    }
    let quote_radius = enzui_iced_theme::text_defaults::shape_radius(|s| s.row);
    let quote_overlay = iced::Color {
        a: app.themes.theme().palette.overlay_hover().a,
        ..body_ink
    };
    let content: Element<'_, Message> = if let Some(original) = message
        .reply_to
        .as_ref()
        .and_then(|id| room.messages.iter().find(|item| &item.id == id))
    {
        column![
            container(
                column![
                    enz_text(&original.sender_name)
                        .color(body_ink)
                        .size(TextSize::Caption)
                        .weight_rung(enzui_iced_theme::WeightRung::Semibold),
                    enz_text(if original.deleted {
                        "Message deleted"
                    } else {
                        &original.body
                    })
                    .color(body_ink)
                    .size(TextSize::Caption)
                    .single_line_elided(),
                ]
                .spacing(3)
            )
            .padding(8)
            .style(move |_| container::Style {
                background: Some(quote_overlay.into()),
                border: iced::Border {
                    radius: quote_radius.into(),
                    ..Default::default()
                },
                ..Default::default()
            }),
            enz_text(body)
                .color(body_ink)
                .size_px(f32::from(app.preferences.text_size)),
        ]
        .spacing(8)
        .into()
    } else {
        enz_text(body)
            .color(body_ink)
            .size_px(f32::from(app.preferences.text_size))
            .into_element()
    };
    let content = if !app.selected_messages.is_empty() {
        content
    } else {
        enzui_iced_widgets::selection_scope::enz_selection_scope(content)
            .id(iced::advanced::widget::Id::from(format!(
                "message-text-{}-{}-{}",
                room.key.account, room.key.room, message.id
            )))
            .document_key((&room.key, &message.id, body))
            .label("Message text")
            .on_copy(Message::Copy)
            .into_element()
    };
    let content: Element<'_, Message> = if !message.deleted && !message.attachments.is_empty() {
        let mut body = if message.body.trim().is_empty() {
            column![]
        } else {
            column![content]
        }
        .spacing(8);
        for attachment in &message.attachments {
            if attachment.mime.starts_with("image/") {
                let preview: Element<'_, Message> =
                    if let Some(asset) = app.image_preview(&room.key, &attachment.id) {
                        enzui_iced_widgets::image::enz_image_handle(asset.handle.clone())
                            .fit(enzui_iced_widgets::image::EnzImageFit::Contain)
                            .hug_picture()
                            .radius(enzui_iced_theme::text_defaults::shape_radius(|s| s.media))
                            .alt_text(&attachment.name)
                            .width(Length::Fill)
                            .height(Length::Fixed(200.0))
                            .into_element()
                    } else {
                        container(
                            enzui_iced_widgets::icon::enz_icon("image")
                                .size(32.0)
                                .tint(body_ink)
                                .into_element(),
                        )
                        .center_x(Length::Fill)
                        .center_y(Length::Fixed(200.0))
                        .into()
                    };
                let preview_key = crate::app::previews::PreviewKey {
                    room: room.key.clone(),
                    id: attachment.id.clone(),
                };
                let shown = preview_key.clone();
                let preview = iced::widget::sensor(preview)
                    .key((
                        app.sessions.get(&room.key.account).copied(),
                        preview_key.clone(),
                    ))
                    .anticipate(160.0)
                    .on_show(move |_| Message::PreviewVisible(shown.clone()))
                    .on_hide(Message::PreviewHidden(preview_key));
                body = body.push(
                    enzui_iced_widgets::pressable::enz_pressable(
                        preview.into(),
                        Message::OpenAttachment(
                            room.key.clone(),
                            message.id.clone(),
                            attachment.id.clone(),
                        ),
                    )
                    .id(WidgetId::from(format!(
                        "messages-preview-{}-{}-{}-{}",
                        room.key.account, room.key.room, message.id, attachment.id
                    )))
                    .tokens(enzui_iced_widgets::pressable::PressableTokens {
                        card_bg: iced::Color::TRANSPARENT,
                        card_border: iced::Color::TRANSPARENT,
                        focus_ring: body_ink,
                        ..enzui_iced_widgets::pressable::PressableTokens::from_palette(
                            &app.themes.theme().palette,
                        )
                    })
                    .border_width(0.0)
                    .radius(enzui_iced_theme::text_defaults::shape_radius(|s| s.media))
                    .width(Length::Fill)
                    .aria_label(format!("Open preview {}", attachment.name))
                    .into_element(),
                );
            }
            body = body.push(
                row![
                    super::shared::themed_file_icon(&attachment.name, &attachment.mime)
                        .unwrap_or_else(|| {
                            enzui_iced_widgets::icon::enz_icon(
                                if attachment.mime.starts_with("image/") {
                                    "image"
                                } else if attachment.mime.starts_with("video/") {
                                    "film"
                                } else if attachment.mime.starts_with("audio/") {
                                    "music"
                                } else {
                                    "file"
                                },
                            )
                            .size(20.0)
                            .tint(body_ink)
                            .into_element()
                        }),
                    enz_text(&attachment.name)
                        .color(body_ink)
                        .size(TextSize::Body)
                        .single_line_elided()
                        .width(Length::Fill),
                    enz_text(
                        if app.attachment_busy(&room.key, &message.id, &attachment.id) {
                            "Loading…".into()
                        } else {
                            file_size(attachment.bytes)
                        }
                    )
                    .size(TextSize::Caption)
                    .color(body_ink),
                    enz_toolbar_icon(
                        "external-link",
                        Message::OpenAttachment(
                            room.key.clone(),
                            message.id.clone(),
                            attachment.id.clone()
                        )
                    )
                    .id(WidgetId::from(format!(
                        "messages-open-file-{}-{}-{}-{}",
                        room.key.account, room.key.room, message.id, attachment.id
                    )))
                    .tokens(attachment_tokens)
                    .aria_label(format!("Open file {}", attachment.name))
                    .tooltip("Open file")
                    .enabled(
                        (attachment.id.starts_with("local:")
                            || app.adapters.contains_key(&room.key.account))
                            && !app.attachment_busy(&room.key, &message.id, &attachment.id)
                    ),
                    enz_toolbar_icon(
                        "download",
                        Message::SaveAttachment(
                            room.key.clone(),
                            message.id.clone(),
                            attachment.id.clone()
                        )
                    )
                    .id(WidgetId::from(format!(
                        "messages-save-file-{}-{}-{}-{}",
                        room.key.account, room.key.room, message.id, attachment.id
                    )))
                    .tokens(attachment_tokens)
                    .aria_label(format!("Save file {}", attachment.name))
                    .tooltip("Save file")
                    .enabled(
                        (attachment.id.starts_with("local:")
                            || app.adapters.contains_key(&room.key.account))
                            && !app.attachment_busy(&room.key, &message.id, &attachment.id)
                    ),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            );
        }
        body.into()
    } else {
        content
    };
    let content: Element<'_, Message> = if message.reactions.is_empty() {
        content
    } else {
        iced::widget::stack![
            content,
            iced::widget::float(
                container(reactions)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .align_y(iced::alignment::Vertical::Bottom)
                    .align_x(if message.own {
                        iced::alignment::Horizontal::Left
                    } else {
                        iced::alignment::Horizontal::Right
                    })
            )
            .translate(|_, _| iced::Vector::new(0.0, 24.0))
        ]
        .into()
    };
    let content = super::picker::anchored(
        app,
        content,
        crate::app::Popup::Reaction(room.key.clone(), message.id.clone()),
        if message.own {
            enzui_iced_core::placement::Alignment::Start
        } else {
            enzui_iced_core::placement::Alignment::End
        },
    );
    let bubble = bubble.custom_body(content, body.to_owned());
    let bubble: Element<'_, Message> = if app.selected_messages.is_empty() {
        bubble.into_element()
    } else {
        enzui_iced_widgets::pressable::enz_pressable(
            row![
                enzui_iced_widgets::icon::enz_icon(
                    if app.selected_messages.contains(&message.id) {
                        "circle-check"
                    } else {
                        "circle"
                    }
                )
                .size(18.0)
                .into_element(),
                bubble.into_element()
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center)
            .into(),
            Message::SelectChatItem(room.key.clone(), message.id.clone()),
        )
        .id(WidgetId::from(format!(
            "messages-select-{}-{}-{}",
            room.key.account, room.key.room, message.id
        )))
        .tokens(enzui_iced_widgets::pressable::PressableTokens {
            card_bg: iced::Color::TRANSPARENT,
            card_border: iced::Color::TRANSPARENT,
            ..enzui_iced_widgets::pressable::PressableTokens::from_palette(
                &app.themes.theme().palette,
            )
        })
        .border_width(0.0)
        .width(Length::Fill)
        .aria_label("Select message")
        .into_element()
    };
    let key = room.key.clone();
    let id = message.id.clone();
    enzui_iced_widgets::context_target::enz_context_target(bubble, move |point| {
        Message::Context(
            crate::app::ContextTarget::Message(key.clone(), id.clone()),
            point,
        )
    })
    .height(Length::Shrink)
    .enabled(!message.deleted)
    .id(iced::advanced::widget::Id::from(format!(
        "messages-action-{}",
        message.id
    )))
    .into_element()
}

fn composer_dock<'a>(app: &'a App, room: &'a Conversation) -> Element<'a, Message> {
    let mut names: Vec<_> = app
        .typing
        .iter()
        .filter(|((key, _), (_, time))| key == &room.key && time.elapsed().as_secs() < 8)
        .map(|(_, (name, _))| name.as_str())
        .collect();
    names.sort_unstable();
    names.dedup();
    let mut dock = column![].width(Length::Fill);
    if !names.is_empty() {
        let status = enzui_iced_widgets::progress_text::enz_progress_text(format!(
            "{} {} typing…",
            names.join(", "),
            if names.len() == 1 { "is" } else { "are" }
        ))
        .size(TextSize::Headline)
        .backdrop(app.themes.theme().palette.surface_elevated)
        .into_element();
        let status = enzui_iced_widgets::surface::enz_surface(status)
            .fill(app.themes.theme().palette.surface_elevated)
            .border_width(0.0)
            .padding([4, 10])
            .radius(999.0)
            .into_element();
        dock = dock.push(container(status).padding(iced::Padding::ZERO.left(28).right(28).top(8)));
    }
    // The composer shares the transcript's reading column (760 px, centred)
    // instead of spanning the pane: a 1,270 px input under a 760 px column of
    // messages read as a separate surface.
    dock.push(
        container(enzui_iced_widgets::max_width::max_width(
            composer(app, room),
            TRANSCRIPT_COLUMN + 48.0,
        ))
        .center_x(Length::Fill),
    )
    .into()
}

/// The transcript's reading width (see `transcript.rs`).
const TRANSCRIPT_COLUMN: f32 = 760.0;

fn composer<'a>(app: &'a App, room: &'a Conversation) -> Element<'a, Message> {
    if !app.selected_messages.is_empty() {
        return container(
            enzui_iced_widgets::glass::enz_glass(
                row![
                    enz_text(format!("{} selected", app.selected_messages.len()))
                        .weight_rung(enzui_iced_theme::WeightRung::Semibold),
                    Space::new().width(Length::Fill),
                    enz_button("Copy", Message::CopySelected)
                        .id(WidgetId::new("messages-copy-selection"))
                        .kind(ButtonKind::Quiet)
                        .icon("copy")
                        .size(ButtonSize::Sm),
                    enz_toolbar_icon("x", Message::ClearMessageSelection)
                        .id(WidgetId::new("messages-finish-selection"))
                        .aria_label("Finish selection")
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center)
                .padding(10),
            )
            .radius(enzui_iced_theme::text_defaults::shape_radius(|s| s.card))
            .into_element(),
        )
        .padding([12, 20])
        .width(Length::Fill)
        .into();
    }
    let capabilities = app.store.capabilities(&room.key);
    if !capabilities.supports(Capability::SendText) {
        return container(hint(
            capabilities
                .get(Capability::SendText)
                .reason()
                .unwrap_or("Messages cannot be sent here."),
        ))
        .padding(20)
        .width(Length::Fill)
        .into();
    }
    let emoji = super::picker::anchored(
        app,
        enz_toolbar_icon("smile", Message::TogglePopup(crate::app::Popup::Emoji))
            .id(iced::advanced::widget::Id::new("messages-emoji"))
            .aria_label("Insert emoji")
            .tooltip("Emoji"),
        crate::app::Popup::Emoji,
        enzui_iced_core::placement::Alignment::Start,
    );
    let attachment_enabled = capabilities.get(Capability::AttachFiles).is_available()
        && app.editing.is_none()
        && !app.file_busy.contains(&room.key)
        && !app.loading
        && !app.persistence_blocked;
    let attachment = enz_toolbar_icon("paperclip", Message::AttachFiles)
        .id(WidgetId::new("messages-attach"))
        .aria_label("Attach a file")
        .tooltip(
            capabilities
                .get(Capability::AttachFiles)
                .reason()
                .unwrap_or("Attach files")
                .to_owned(),
        )
        .enabled(attachment_enabled);
    let attachment = iced::widget::mouse_area(attachment).interaction(if attachment_enabled {
        iced::mouse::Interaction::Pointer
    } else {
        iced::mouse::Interaction::Idle
    });
    let send_enabled = (!app.editor.text().trim().is_empty() || !room.draft_attachments.is_empty())
        && !app.loading
        && !app.persistence_blocked
        && !app.file_busy.contains(&room.key);
    let composer = enz_text_area(&app.editor, Message::Editor)
        .placeholder("Write a message…")
        .aria_label("Message text")
        .id(iced::advanced::widget::Id::new("messages-composer"))
        .auto_grow(1, 7)
        .read_only(app.loading || app.persistence_blocked || app.stopping())
        .radius(enzui_iced_theme::text_defaults::shape_radius(|s| s.field))
        .font_from_theme(app.themes.theme())
        .on_submit(Message::Send)
        .variant(enzui_iced_widgets::text_area::TextAreaVariant::Plain);
    let actions = row![emoji]
        .extend(
            capabilities
                .supports(Capability::AttachFiles)
                .then_some(attachment.into()),
        )
        .push(Space::new().width(Length::Fill))
        .push(
            iced::widget::mouse_area(
                enz_toolbar_icon(
                    if app.editing.is_some() {
                        "check"
                    } else {
                        "send"
                    },
                    Message::Send,
                )
                .id(WidgetId::new("messages-send"))
                .aria_label(if app.editing.is_some() {
                    "Save edit"
                } else {
                    "Send message"
                })
                .tooltip(if app.editing.is_some() {
                    "Save edit"
                } else {
                    "Send message"
                })
                .enabled(send_enabled),
            )
            .interaction(if send_enabled {
                iced::mouse::Interaction::Pointer
            } else {
                iced::mouse::Interaction::Idle
            }),
        )
        .spacing(4)
        .align_y(iced::Alignment::Center);
    let mut draft = column![].spacing(4);
    for file in &room.draft_attachments {
        let attachment = row![
            if let Some(asset) = app.image_preview(&room.key, &format!("local:{}", file.id)) {
                enzui_iced_widgets::image::enz_image_handle(asset.handle.clone())
                    .fit(enzui_iced_widgets::image::EnzImageFit::Cover)
                    .radius(enzui_iced_theme::text_defaults::shape_radius(|s| s.media))
                    .alt_text(&file.name)
                    .width(Length::Fixed(48.0))
                    .height(Length::Fixed(48.0))
                    .into_element()
            } else if let Some(art) = super::shared::themed_file_icon(&file.name, &file.mime) {
                art
            } else {
                enzui_iced_widgets::icon::enz_icon(if file.mime.starts_with("image/") {
                    "image"
                } else {
                    "file"
                })
                .size(if file.mime.starts_with("image/") {
                    28.0
                } else {
                    16.0
                })
                .into_element()
            },
            enz_text(&file.name)
                .size(TextSize::Caption)
                .single_line_elided()
                .width(Length::Fill),
            enz_text(file_size(file.bytes))
                .size(TextSize::Caption)
                .tone(TextTone::Muted),
            enz_toolbar_icon("x", Message::RemoveAttachment(file.id))
                .id(WidgetId::from(format!("messages-remove-file-{}", file.id)))
                .aria_label(format!("Remove file {}", file.name))
                .tooltip("Remove file")
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center);
        draft = draft.push(
            enzui_iced_widgets::file_drag::enz_file_drag_source(attachment, [file.path.clone()])
                .temporary_files(true)
                .key((&room.key, file.id))
                .allows_captured_press(true),
        );
    }
    if app.file_busy.contains(&room.key) {
        draft = draft.push(hint("Preparing attachments…"));
    }
    let mut contents = column![].spacing(4);
    if !room.draft_attachments.is_empty() || app.file_busy.contains(&room.key) {
        contents =
            contents.push(container(draft).padding(iced::Padding::ZERO.left(12).right(8).top(8)));
    }
    let composer = container(
        contents
            .push(composer.into_element())
            .push(container(actions).padding(iced::Padding::ZERO.left(8).right(8).bottom(6))),
    )
    .width(Length::Fill);
    let context = if let Some(editing) = &app.editing {
        Some(format!("Editing: {}", editing.body))
    } else {
        room.reply_to
            .as_ref()
            .and_then(|id| room.messages.iter().find(|message| &message.id == id))
            .map(|message| format!("Replying to {}: {}", message.sender_name, message.body))
    };
    let mut content = column![].spacing(8);
    if let Some(context) = context {
        content = content.push(
            row![
                enz_text(context)
                    .size(TextSize::Caption)
                    .single_line_elided()
                    .width(Length::Fill),
                enz_toolbar_icon("x", Message::CancelReply)
                    .id(WidgetId::new("messages-cancel-reply"))
                    .aria_label("Cancel reply or edit")
                    .tooltip("Cancel")
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
        );
    }
    content = content.push(
        iced::widget::mouse_area(composer_surface(
            composer.into(),
            enzui_iced_theme::text_defaults::shape_radius(|s| s.card),
            Length::Fill,
        ))
        .on_press(Message::FocusComposer)
        .on_scroll(|_| Message::ComposerScrollBoundary)
        .interaction(iced::mouse::Interaction::Idle),
    );
    let session = app.session(room.key.account);
    let room = room.key.clone();
    let drop_key = (session, room.clone());
    enzui_iced_widgets::file_drop::enz_file_drop_target_with_receipt(
        container(content).padding([12, 20]).width(Length::Fill),
        move |drop| Message::FilesDropped(session, room.clone(), drop),
    )
    .copy_only(true)
    .enabled(attachment_enabled)
    .key(drop_key)
    .into()
}

fn file_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

pub(super) fn bubble_fill(app: &App, name: &str) -> iced::Color {
    match name {
        "System surface" => app.themes.theme().palette.surface_alt,
        "Ocean" => iced::Color::from_rgb8(21, 104, 163),
        "Forest" => iced::Color::from_rgb8(31, 112, 76),
        "Plum" => iced::Color::from_rgb8(115, 67, 149),
        "Neutral" => iced::Color::from_rgb8(78, 83, 93),
        custom if custom.starts_with('#') && custom.len() == 7 => {
            u32::from_str_radix(&custom[1..], 16)
                .ok()
                .map(|rgb| iced::Color::from_rgb8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8))
                .unwrap_or(app.themes.theme().palette.accent)
        }
        _ => app.themes.theme().palette.accent,
    }
}

pub(super) fn bubble_tokens(app: &App) -> ChatBubbleTokens {
    let palette = &app.themes.theme().palette;
    let fill = super::bubble_contrast::selection_distinct(
        bubble_fill(app, &app.preferences.bubble_color),
        palette.accent,
    );
    let pair = palette.filled_color(fill);
    let received = if app.preferences.received_bubble_color == "System surface" {
        (palette.surface_alt, palette.text)
    } else {
        let pair = palette.filled_color(bubble_fill(app, &app.preferences.received_bubble_color));
        (pair.color, pair.text)
    };
    ChatBubbleTokens {
        user_background: pair.color,
        user_foreground: pair.text,
        assistant_background: received.0,
        assistant_foreground: received.1,
        bubble_padding_y: 7.0,
        bubble_min_height: 32.0,
        bubble_radius: app.preferences.bubble_radius,
        body_size: f32::from(app.preferences.text_size),
        shadow_color: iced::Color::TRANSPARENT,
        shadow_blur: 0.0,
        shadow_offset_y: 0.0,
        ..ChatBubbleTokens::from_palette(palette)
    }
}

fn composer_surface(
    content: Element<'_, Message>,
    radius: f32,
    width: Length,
) -> Element<'_, Message> {
    enzui_iced_widgets::surface::enz_surface(content)
        .fill_resolved(|theme| theme.palette().background.weak.color)
        .material(enzui_iced_theme::MaterialStyle::FlatFrosted)
        .material_ink(enzui_iced_theme::MaterialInk::Protected)
        .material_tint_opacity(0.72)
        .padding(8)
        .width(width)
        .border_width(0.0)
        .radius(radius)
        .elevation(0)
        .into_element()
}
