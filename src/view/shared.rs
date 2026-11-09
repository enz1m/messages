use std::collections::HashSet;

use enzui_iced_widgets::icon::enz_icon;
use enzui_iced_widgets::scroll_view::enz_scroll_view;
use enzui_iced_widgets::view_switcher::{SwitcherPage, enz_view_switcher};
use iced::widget::column;
use messages_core::Conversation;

use super::*;
use crate::app::SharedSection;

pub(super) struct SharedItem {
    pub identity: String,
    pub message_id: String,
    pub label: String,
    pub detail: String,
    pub icon: &'static str,
    // A shared file's name and type, drawn with the icon theme's artwork.
    pub file: Option<(String, String)>,
}

// Theme artwork beside an attachment's name.
const FILE_ART: f32 = 24.0;

// A file's icon from the desktop icon theme, or `None` when the theme has none
// (the caller keeps its glyph). A generic or missing type defers to the name.
pub(super) fn themed_file_icon<'a, M: 'a>(name: &str, mime: &str) -> Option<Element<'a, M>> {
    use enzui_iced_widgets::file_icon::FileIconQuery;
    let query = match mime {
        "" | "application/octet-stream" => FileIconQuery::file_name(name),
        mime => FileIconQuery::mime(mime),
    };
    enzui_iced_widgets::file_icon::themed_file_icon(&query, FILE_ART)
        .map(|art| art.into_element(FILE_ART))
}

pub(super) fn items(room: &Conversation, section: SharedSection) -> Vec<SharedItem> {
    let mut items = Vec::new();
    let mut seen = HashSet::new();
    for message in room
        .messages
        .iter()
        .rev()
        .filter(|message| !message.deleted)
    {
        if section == SharedSection::Links {
            for word in message.body.split_whitespace() {
                let url = word.trim_matches(|c: char| {
                    matches!(
                        c,
                        '(' | ')' | '[' | ']' | '<' | '>' | '"' | ',' | '.' | ';' | '!'
                    )
                });
                if (url.starts_with("https://") || url.starts_with("http://"))
                    && seen.insert(url.to_owned())
                {
                    items.push(SharedItem {
                        identity: url.to_owned(),
                        message_id: message.id.clone(),
                        label: url.to_owned(),
                        detail: message.sender_name.clone(),
                        icon: "link",
                        file: None,
                    });
                }
            }
        } else {
            for file in &message.attachments {
                let media = file.mime.starts_with("image/") || file.mime.starts_with("video/");
                if media != (section == SharedSection::Media) {
                    continue;
                }
                items.push(SharedItem {
                    identity: file.id.clone(),
                    message_id: message.id.clone(),
                    label: file.name.clone(),
                    detail: format!(
                        "{} · {}",
                        message.sender_name,
                        if file.bytes >= 1_000_000 {
                            format!("{:.1} MB", file.bytes as f64 / 1_000_000.0)
                        } else {
                            format!("{} KB", file.bytes.div_ceil(1000))
                        }
                    ),
                    icon: if file.mime.starts_with("image/") {
                        "image"
                    } else if file.mime.starts_with("video/") {
                        "film"
                    } else {
                        "file"
                    },
                    file: Some((file.name.clone(), file.mime.clone())),
                });
            }
        }
    }
    items
}

pub(super) fn details(app: &App) -> Element<'_, Message> {
    let Some(room) = app.current_room() else {
        return hint("Choose a conversation first.");
    };
    let identity = column![
        room_avatar(app, room, 64.0),
        enz_text(&room.title)
            .size(TextSize::Title3)
            .weight_rung(enzui_iced_theme::WeightRung::Semibold),
        hint(&room.subtitle),
    ]
    .spacing(8)
    .align_x(iced::Alignment::Center)
    .width(Length::Fill);
    let actions = row![
        column![
            super::picker::mute_button(app, room, true),
            hint(if room.muted { "Unmute" } else { "Mute" })
        ]
        .spacing(6)
        .align_x(iced::Alignment::Center)
        .width(Length::Fill),
        detail_action(
            "messages-details-pin",
            "pin",
            if room.pinned { "Unpin" } else { "Pin" },
            Message::Pin
        ),
        detail_action(
            "messages-details-archive",
            "archive",
            if room.archived {
                "Unarchive"
            } else {
                "Archive"
            },
            Message::Archive
        ),
    ]
    .spacing(12)
    .width(Length::Fill);
    let pages = [
        (SharedSection::Media, "media", "Media"),
        (SharedSection::Links, "links", "Links"),
        (SharedSection::Files, "files", "Files"),
    ]
    .into_iter()
    .map(|(section, key, label)| {
        // A count only when there is something to count: "Media 0" on every
        // tab of an empty conversation was noise.
        let count = items(room, section).len();
        SwitcherPage::new(
            key,
            if count == 0 {
                label.to_owned()
            } else {
                format!("{label} {count}")
            },
        )
    })
    .collect();
    let tabs = enz_view_switcher(
        pages,
        match app.shared_section {
            SharedSection::Media => 0,
            SharedSection::Links => 1,
            SharedSection::Files => 2,
        },
        |index| {
            Message::SharedSection(match index {
                1 => SharedSection::Links,
                2 => SharedSection::Files,
                _ => SharedSection::Media,
            })
        },
    )
    .stretch(true)
    .button_height(30.0)
    .aria_label("Shared content from loaded messages");
    let entries = items(room, app.shared_section);
    let mut list = column![].spacing(6);
    if entries.is_empty() {
        list = list.push(
            container(
                column![
                    enz_icon(match app.shared_section {
                        SharedSection::Media => "images",
                        SharedSection::Links => "link",
                        SharedSection::Files => "files",
                    })
                    .size(28.0)
                    .tint(app.themes.theme().palette.text_subtle)
                    .into_element(),
                    hint(match app.shared_section {
                        SharedSection::Media => "No shared photos or videos yet",
                        SharedSection::Links => "No shared links yet",
                        SharedSection::Files => "No shared files yet",
                    }),
                ]
                .spacing(12)
                .align_x(iced::Alignment::Center),
            )
            .padding([32, 12])
            .width(Length::Fill)
            .align_x(iced::alignment::Horizontal::Center),
        );
    }
    for item in entries {
        let label = item.label.clone();
        let id = WidgetId::from(format!(
            "messages-shared-{}-{}-{}-{}",
            room.key.account, room.key.room, item.message_id, item.identity
        ));
        list = list.push(
            enzui_iced_widgets::focus_button::focus_button(
                row![
                    item.file
                        .as_ref()
                        .and_then(|(name, mime)| themed_file_icon(name, mime))
                        .unwrap_or_else(|| enz_icon(item.icon)
                            .size(22.0)
                            .tint(app.themes.theme().palette.accent)
                            .into_element()),
                    column![
                        enz_text(item.label)
                            .size(TextSize::Body)
                            .single_line_elided()
                            .width(Length::Fill),
                        hint(item.detail)
                    ]
                    .spacing(4)
                    .width(Length::Fill),
                    enz_icon("navigation.forward").size(14.0).into_element(),
                ]
                .spacing(12)
                .align_y(iced::Alignment::Center),
                Message::RevealShared(item.message_id),
            )
            .id(id)
            .semantics(
                enzui_iced_core::semantics::Semantics::new(
                    enzui_iced_core::semantics::Role::Button,
                )
                .label(label)
                .description("Show this shared item in its conversation"),
            )
            .padding(10)
            .width(Length::Fill)
            .style_focus(|theme, status| {
                iced::widget::button::text(
                    theme,
                    match status {
                        enzui_iced_widgets::focus_button::FocusStatus::Hovered => {
                            iced::widget::button::Status::Hovered
                        }
                        enzui_iced_widgets::focus_button::FocusStatus::Pressed => {
                            iced::widget::button::Status::Pressed
                        }
                        _ => iced::widget::button::Status::Active,
                    },
                )
            }),
        );
    }
    column![
        identity,
        actions,
        tabs,
        enz_scroll_view(list).height(Length::Shrink.max(240.0))
    ]
    .spacing(18)
    .into()
}

fn detail_action<'a>(
    id: &'static str,
    icon: &str,
    label: &'a str,
    message: Message,
) -> Element<'a, Message> {
    column![
        enz_toolbar_icon(icon, message)
            .id(WidgetId::new(id))
            .size(36.0, 18.0)
            .aria_label(label)
            .tooltip(label),
        hint(label)
    ]
    .spacing(5)
    .align_x(iced::Alignment::Center)
    .width(Length::Fill)
    .into()
}
