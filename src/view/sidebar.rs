use enzui_iced_widgets::badge::{BadgeSize, BadgeTone, enz_badge};
use enzui_iced_widgets::tree_view::{EnzTreeItem, enz_tree_view};
use iced::widget::column;

use super::*;
use crate::app::Filter;

pub(super) fn conversation_list(
    app: &App,
    width: Length,
    focus_search_on_mount: bool,
    available_width: f32,
) -> Element<'_, Message> {
    if app.loading {
        return Space::new().width(width).height(Length::Fill).into();
    }
    let compact = app.window_size.width < 760.0;
    let visible = app.visible_rooms();
    let rooms = ordered_rooms(visible);
    let heading = enzui_iced_widgets::search_field::enz_search_field(&app.search, Message::Search)
        .placeholder(text("messages-search"))
        .id(iced::advanced::widget::Id::new("messages-search"))
        .on_clear(Message::Search(String::new()))
        .focus_on_mount(focus_search_on_mount)
        .size(enzui_iced_widgets::text_field::TextFieldSize::Sm)
        .radius(999.0)
        .chrome(enzui_iced_widgets::search_field::SearchFieldChrome::Ghost);
    let items: Vec<EnzTreeItem> = rooms
        .iter()
        .map(|room| {
            let level = u8::from(room.parent_room.as_ref().is_some_and(|parent| {
                rooms
                    .iter()
                    .any(|other| other.key.account == room.key.account && other.key.room == *parent)
            }));
            let children = rooms.iter().any(|other| {
                other.key.account == room.key.account
                    && other.parent_room.as_ref() == Some(&room.key.room)
            });
            let item = if children {
                EnzTreeItem::branch(&room.title, level, !app.collapsed_rooms.contains(&room.key))
            } else {
                EnzTreeItem::leaf(&room.title, level)
            };
            item.key(format!("{}:{}", room.key.account, room.key.room))
        })
        .collect();
    let selected = rooms
        .iter()
        .position(|room| app.selected.as_ref() == Some(&room.key));
    let select_keys: Vec<_> = rooms.iter().map(|room| room.key.clone()).collect();
    let context_keys = select_keys.clone();
    let toggle_keys = select_keys.clone();
    let rows: Vec<_> = rooms
        .iter()
        .map(|room| ConversationRow {
            key: room.key.clone(),
            session: app.session(room.key.account),
            title: room.title.clone(),
            preview: if !app.search.trim().is_empty() {
                room.readable_messages()
                    .rev()
                    .find(|message| {
                        !message.deleted
                            && message
                                .body
                                .to_lowercase()
                                .contains(&app.search.trim().to_lowercase())
                    })
                    .map(|message| message.body.clone())
                    .unwrap_or_else(|| room.preview().to_owned())
            } else if room.draft.is_empty() {
                room.preview().to_owned()
            } else {
                format!("Draft: {}", room.draft)
            },
            time: room
                .updated_at()
                .map(|time| {
                    time.with_timezone(&chrono::Local)
                        .format("%H:%M")
                        .to_string()
                })
                .unwrap_or_default(),
            unread: room.unread,
            muted: room.muted,
            avatar: avatar_tokens(app, &room.title),
            photo: app.avatar_image(&room.key),
            // The selected row's fill is the sidebar tint (as in every app's
            // sidebar); its inks are checked against that fill.
            ink: if app.selected.as_ref() == Some(&room.key) {
                enzui_iced_theme::Palette::with_contrast_on(
                    selected_fill(app),
                    app.themes.theme().palette.text,
                    4.5,
                )
            } else {
                app.themes.theme().palette.text
            },
            muted_ink: if app.selected.as_ref() == Some(&room.key) {
                app.themes.theme().palette.text_muted_on(selected_fill(app))
            } else {
                app.themes.theme().palette.text_muted
            },
            icon: if room.parent_room.is_some() {
                Some("messages-square")
            } else if room.kind == messages_core::RoomKind::Channel {
                Some("hash")
            } else {
                None
            },
        })
        .collect();
    let row_theme = app.themes.theme().clone();
    let list = enz_tree_view(items)
        .aria_label("Conversations")
        .id_prefix("messages-conversations")
        .scroll_id(WidgetId::new("messages-conversations-scroll"))
        .scroll_offset(app.sidebar_scroll_offset)
        .on_scroll(Message::SidebarScrolled)
        .selected(selected)
        .tree_row_height(62.0)
        .row_padding_x(6.0)
        .disclosure_size(12.0)
        .indent_width(14.0)
        .panel_border_width(0.0)
        .panel_radius(0.0)
        .row_radius(enzui_iced_theme::text_defaults::shape_radius(|s| s.row))
        .surface_background(iced::Color::TRANSPARENT)
        .empty_text(if app.search.is_empty() {
            "No conversations here yet"
        } else {
            "No messages match your search"
        })
        .row_on_context(move |index, point| {
            Message::Context(
                crate::app::ContextTarget::Conversation(context_keys[index].clone()),
                point,
            )
        })
        .on_toggle(move |index| Message::ToggleRoomBranch(toggle_keys[index].clone()))
        .on_select(move |index| Message::Select(select_keys[index].clone()))
        .row_content(move |index, _| {
            let _scope = enzui_iced_theme::text_defaults::TextDefaultsScope::enter(&row_theme);
            let row = &rows[index];
            let key = row.key.clone();
            let shown = key.clone();
            let session = row.session;
            iced::widget::sensor(conversation_row(row))
                .key((session, key.clone()))
                .anticipate(80.0)
                .on_show(move |_| Message::AvatarVisibility(session, shown.clone(), true))
                .on_hide(Message::AvatarVisibility(session, key, false))
                .into()
        })
        .into_element();
    enzui_iced_widgets::sidebar_content::enz_sidebar_content()
        .spacing_scale(app.themes.theme().spacing)
        .padding_left(enzui_iced_theme::SpaceToken::Xs)
        .padding_right(enzui_iced_theme::SpaceToken::Xs)
        .padding_top_px(0.0)
        .padding_bottom_px(0.0)
        .spacing_px(0.0)
        .push(
            container(container(heading).style(move |theme: &iced::Theme| {
                container::Style {
                    background: Some(
                        if compact {
                            theme.palette().background.weak.color
                        } else {
                            theme.palette().background.base.color
                        }
                        .into(),
                    ),
                    border: iced::Border {
                        radius: 999.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            }))
            .padding([
                app.themes.theme().spacing.space5,
                app.themes.theme().spacing.space4,
            ]),
        )
        .push(
            container(list)
                .padding([
                    app.themes.theme().spacing.space2,
                    app.themes.theme().spacing.space1,
                ])
                .height(Length::Fill),
        )
        .push(account_dock(app, available_width))
        .width(width)
        .into_element()
}

struct ConversationRow {
    key: messages_core::RoomKey,
    session: crate::app::Session,
    title: String,
    preview: String,
    time: String,
    unread: u32,
    muted: bool,
    avatar: AvatarTokens,
    photo: Option<iced::widget::image::Handle>,
    ink: iced::Color,
    muted_ink: iced::Color,
    icon: Option<&'static str>,
}

fn conversation_row(room: &ConversationRow) -> Element<'static, Message> {
    let title = enz_text(room.title.clone())
        .color(room.ink)
        .size(TextSize::Body)
        .weight_rung(if room.unread > 0 {
            enzui_iced_theme::WeightRung::Semibold
        } else {
            enzui_iced_theme::WeightRung::Medium
        })
        .single_line_elided()
        .width(Length::Fill);
    let top = row![
        title,
        if room.muted {
            enzui_iced_widgets::icon::enz_icon("bell-off")
                .size(12.0)
                .tint(room.muted_ink)
                .into_element()
        } else {
            Space::new().into()
        },
        enz_text(room.time.clone())
            .size(TextSize::Caption)
            .color(room.muted_ink)
    ]
    .spacing(8)
    .align_y(iced::Alignment::Center);
    let bottom = row![
        enz_text(room.preview.clone())
            .size(TextSize::Caption)
            .color(room.muted_ink)
            .wrap(true)
            .lines(2)
            .width(Length::Fill),
        if room.unread > 0 {
            enz_badge(&room.unread.to_string())
                .size(BadgeSize::Sm)
                .tone(if room.muted {
                    BadgeTone::Default
                } else {
                    BadgeTone::AccentStrong
                })
                .into_element()
        } else {
            Space::new().into()
        },
    ]
    .spacing(6)
    .align_y(iced::Alignment::Center);
    row![
        if let Some(icon) = room.icon {
            container(
                enzui_iced_widgets::icon::enz_icon(icon)
                    .size(21.0)
                    .tint(room.ink)
                    .into_element(),
            )
            .center(38.0)
            .into()
        } else {
            enz_avatar(&room.title)
                .avatar_size(44.0)
                .tokens(room.avatar)
                .media(
                    room.photo
                        .clone()
                        .map(enzui_iced_widgets::avatar::AvatarMedia::Image)
                        .unwrap_or_default(),
                )
                .into_element()
        },
        column![top, bottom].spacing(1).width(Length::Fill),
    ]
    .spacing(6)
    .align_y(iced::Alignment::Center)
    .width(Length::Fill)
    .into()
}

fn account_dock(app: &App, available_width: f32) -> Element<'_, Message> {
    use enzui_iced_widgets::menu::{EnzMenuItem, enz_menu};
    let control = enzui_iced_widgets::toolbar_icon::TitlebarAffordanceTokens::DEFAULT
        .effective_control_size();
    let footer_width = (available_width - 2.0 * app.themes.theme().spacing.space2 - 8.0).max(0.0);
    let slots = (footer_width / (control + 4.0)).floor() as usize;
    let account_slots = slots.saturating_sub(3);
    let overflow = app.store.accounts.len() > account_slots;
    let visible =
        enzui_iced_widgets::action_bar::inline_budget(account_slots, app.store.accounts.len());
    let mut accounts = row![
        enz_toolbar_icon("layers", Message::AccountFilter(None))
            .id(WidgetId::new("messages-filter-all-accounts"))
            .active(app.account_filter.is_none())
            .aria_label("All accounts")
            .tooltip("All accounts")
    ]
    .spacing(4)
    .align_y(iced::Alignment::Center);
    for account in app.store.accounts.iter().take(visible) {
        let icon = app
            .registry
            .descriptor(account.protocol.as_str())
            .map(|p| p.icon)
            .unwrap_or("user");
        accounts = accounts.push(
            enz_toolbar_icon(icon, Message::AccountFilter(Some(account.id)))
                .id(WidgetId::from(format!(
                    "messages-filter-account-{}",
                    account.id
                )))
                .active(app.account_filter == Some(account.id))
                .aria_label(format!("{}: {}", account.label, account.user_id))
                .tooltip(format!("{} · {}", account.label, account.user_id)),
        );
    }
    if overflow {
        let open = matches!(app.popup, Some(crate::app::Popup::Accounts));
        let token = WidgetId::new("messages-accounts-return");
        let trigger = enz_toolbar_icon(
            "ellipsis",
            Message::TogglePopup(crate::app::Popup::Accounts),
        )
        .id(WidgetId::new("messages-account-overflow"))
        .active(app.account_filter.is_some_and(|id| {
            app.store
                .accounts
                .iter()
                .skip(visible)
                .any(|account| account.id == id)
        }))
        .aria_label("More accounts")
        .tooltip("More accounts")
        .relinquish_focus_while(open)
        .reclaim_focus_on_return((!open).then_some(token.clone()))
        .into_element();
        let menu = matches!(app.popup, Some(crate::app::Popup::Accounts)).then(|| {
            let mut menu = enz_menu()
                .label("More accounts")
                .id_prefix("messages-account-menu")
                .roving_self(true)
                .focus_return_to(token);
            for account in app.store.accounts.iter().skip(visible) {
                let icon = app
                    .registry
                    .descriptor(account.protocol.as_str())
                    .map(|p| p.icon)
                    .unwrap_or("user");
                menu = menu.push_item(
                    EnzMenuItem::command(
                        account.id.to_string(),
                        format!("{} · {}", account.label, account.user_id),
                        Message::AccountFilter(Some(account.id)),
                    )
                    .icon(icon),
                );
            }
            menu.into_element()
        });
        accounts = accounts.push(super::picker::anchored_menu(
            trigger,
            menu,
            enzui_iced_core::placement::Side::Top,
        ));
    }
    container(
        accounts
            .push(Space::new().width(Length::Fill))
            .push(
                enz_toolbar_icon(
                    "mail",
                    Message::Filter(if app.filter == Filter::Unread {
                        Filter::All
                    } else {
                        Filter::Unread
                    }),
                )
                .id(WidgetId::new("messages-filter-unread"))
                .active(app.filter == Filter::Unread)
                .aria_label("Unread conversations")
                .tooltip("Unread"),
            )
            .push(
                enz_toolbar_icon(
                    "archive",
                    Message::Filter(if app.filter == Filter::Archived {
                        Filter::All
                    } else {
                        Filter::Archived
                    }),
                )
                .id(WidgetId::new("messages-filter-archived"))
                .active(app.filter == Filter::Archived)
                .aria_label("Archived conversations")
                .tooltip("Archived"),
            ),
    )
    .padding([10, 4])
    .width(Length::Fill)
    .into()
}

fn ordered_rooms(visible: Vec<&messages_core::Conversation>) -> Vec<&messages_core::Conversation> {
    let mut result = Vec::with_capacity(visible.len());
    for room in &visible {
        if room.parent_room.as_ref().is_some_and(|parent| {
            visible
                .iter()
                .any(|other| other.key.account == room.key.account && other.key.room == *parent)
        }) {
            continue;
        }
        result.push(*room);
        result.extend(visible.iter().copied().filter(|child| {
            child.key.account == room.key.account
                && child.parent_room.as_ref() == Some(&room.key.room)
        }));
    }
    result
}

/// The sidebar's selected-row fill composited over the sidebar surface.
fn selected_fill(app: &App) -> iced::Color {
    let p = &app.themes.theme().palette;
    enzui_iced_widgets::accessibility_metrics::composite_over(
        enzui_iced_widgets::sidebar_row::selected_tint(p),
        p.surface_alt,
    )
}
