use super::*;
use enzui_iced_core::virtual_sequence::{ItemProvider, KeyedProvider, SequenceKey};
use enzui_iced_widgets::virtual_list::enz_virtual_list_variable;
use messages_core::Conversation;
use std::rc::Rc;
struct Rows<'a> {
    room: &'a Conversation,
    messages: Rc<[usize]>,
}
impl ItemProvider<usize> for Rows<'_> {
    fn len(&self) -> u64 {
        self.messages.len() as u64 + 2
    }
    fn item(&self, index: u64) -> Option<usize> {
        (index < self.len()).then_some(index as usize)
    }
}
impl KeyedProvider<usize> for Rows<'_> {
    fn key(&self, index: u64) -> Option<SequenceKey> {
        if index == 0 {
            Some(SequenceKey::new(format!(
                "{}:{}:header",
                self.room.key.account, self.room.key.room
            )))
        } else if index == self.len() - 1 {
            Some(SequenceKey::new(format!(
                "{}:{}:footer",
                self.room.key.account, self.room.key.room
            )))
        } else {
            self.messages
                .get(index as usize - 1)
                .and_then(|&i| self.room.messages.get(i))
                .map(|m| {
                    SequenceKey::new(format!(
                        "{}:{}:m:{}",
                        self.room.key.account, self.room.key.room, m.id
                    ))
                })
        }
    }
}
pub(crate) fn view<'a>(
    app: &'a App,
    room: &'a Conversation,
    compact: bool,
    geometry: &enzui_iced_widgets::app_shell::ShellGeometry,
) -> Element<'a, Message> {
    let gap = app.themes.theme().spacing.space2;
    let mut rail = geometry.scrollbar_safe_area();
    rail.top = if app.search_hit.is_some() || app.locked_history(room) {
        gap
    } else {
        rail.top + gap
    };
    rail.bottom = rail.bottom.max(app.composer_height + gap);
    build(
        app,
        room,
        compact,
        if app.locked_history(room) {
            gap.max(1.0)
        } else {
            geometry.content_inset[0]
        },
        rail,
        None,
    )
}
#[cfg(test)]
pub(crate) fn counted_view<'a>(
    app: &'a App,
    room: &'a Conversation,
    count: Rc<std::cell::Cell<usize>>,
) -> Element<'a, Message> {
    build(app, room, false, 48.0, iced::Padding::ZERO, Some(count))
}
fn build<'a>(
    app: &'a App,
    room: &'a Conversation,
    compact: bool,
    inset: f32,
    rail: iced::Padding,
    probe: Option<std::rc::Rc<std::cell::Cell<usize>>>,
) -> Element<'a, Message> {
    let state = app.scroll_positions.get(&room.key);
    let indices = state.map_or_else(
        || {
            room.messages
                .iter()
                .enumerate()
                .filter(|(_, m)| m.is_readable())
                .map(|(i, _)| i)
                .collect::<Rc<[usize]>>()
        },
        |p| p.layout.messages.clone(),
    );
    let rows = Rows {
        room,
        messages: indices.clone(),
    };
    let heights: Vec<_> = (0..indices.len() + 2)
        .map(|i| state.map_or(72.0, |p| p.layout.model.height_of(i)))
        .collect();
    let viewport = state
        .map_or(app.window_size.height, |p| p.viewport_height)
        .max(1.0);
    let offset = state.map_or_else(
        || (heights.iter().sum::<f32>() - viewport).max(0.0),
        |p| p.y,
    );
    let generation = state.map_or(uuid::Uuid::nil(), |p| p.layout.generation);
    let scroll_key = room.key.clone();
    let measure_key = room.key.clone();
    let measured_rows = indices.clone();
    let list = enz_virtual_list_variable(Rc::new(rows), heights, move |_, index| {
        if let Some(count) = &probe {
            count.set(count.get() + 1);
        }
        let row: Element<'a, Message> = if *index == 0 {
            let mut header = iced::widget::column![Space::new().height(inset)];
            if room.has_older {
                header = header.push(
                    container(
                        enz_button(
                            if app.history_busy.contains(&room.key) {
                                "Loading older messages…"
                            } else {
                                "Load older messages"
                            },
                            Message::LoadOlder(room.key.clone()),
                        )
                        .id(WidgetId::new("messages-load-older"))
                        .kind(ButtonKind::Quiet)
                        .enabled(!app.history_busy.contains(&room.key)),
                    )
                    .center_x(Length::Fill),
                );
            }
            if let Some(error) = app.history_errors.get(&room.key) {
                header = header.push(hint(error));
            }
            header.into()
        } else if *index == indices.len() + 1 {
            Space::new().height(app.composer_height + 8.0).into()
        } else {
            let i = *index - 1;
            let message = &room.messages[indices[i]];
            let date = message.timestamp.with_timezone(&chrono::Local).date_naive();
            let mut content = iced::widget::column![];
            if i == 0
                || room.messages[indices[i - 1]]
                    .timestamp
                    .with_timezone(&chrono::Local)
                    .date_naive()
                    != date
            {
                let local = message.timestamp.with_timezone(&chrono::Local);
                let label = if date == chrono::Local::now().date_naive() {
                    "Today".into()
                } else if Some(date) == chrono::Local::now().date_naive().pred_opt() {
                    "Yesterday".into()
                } else {
                    local.format("%A, %-d %B").to_string()
                };
                content = content.push(
                    container(hint(label))
                        .center_x(Length::Fill)
                        .padding([10, 0]),
                );
            }
            let grouped = app.preferences.group_messages
                && indices
                    .get(i + 1)
                    .map(|&next| &room.messages[next])
                    .is_some_and(|next| {
                        next.sender == message.sender
                            && next.own == message.own
                            && !next.deleted
                            && !message.deleted
                            && (0..=120).contains(
                                &next
                                    .timestamp
                                    .signed_duration_since(message.timestamp)
                                    .num_seconds(),
                            )
                            && next.timestamp.date_naive() == message.timestamp.date_naive()
                    });
            content
                .push(super::conversation::bubble(app, room, message, !grouped))
                .push(Space::new().height(6.0))
                .into()
        };
        container(enzui_iced_widgets::max_width::max_width(row, 760.0))
            .center_x(Length::Fill)
            .padding([0, if compact { 16 } else { 24 }])
            .into()
    })
    .id(crate::app::history::transcript_id(&room.key))
    .scrollbar_insets(rail)
    .viewport_height(viewport)
    .scroll_offset(offset)
    .overscan(3)
    .on_scroll(move |viewport| Message::TranscriptScrolled(scroll_key.clone(), viewport))
    .on_measure(move |measurement| {
        let row = match measurement.index {
            0 => "header".to_owned(),
            index if index > measured_rows.len() => "footer".to_owned(),
            index => format!("m:{}", room.messages[measured_rows[index - 1]].id),
        };
        Message::TranscriptMeasured(measure_key.clone(), generation, row, measurement)
    })
    .into_element();
    let key = room.key.clone();
    iced::widget::sensor(list)
        .key(key.clone())
        .on_resize(move |size| Message::TranscriptViewport(key.clone(), size))
        .into()
}
