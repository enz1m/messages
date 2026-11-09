use super::{App, Message};
use enzui_iced_core::virtual_sequence::VariableHeightModel;
use enzui_iced_widgets::row_measure::RowMeasurement;
use iced::advanced::widget::operation::Animation;
use iced::{Task, widget::scrollable::AbsoluteOffset};
use messages_core::RoomKey;
use std::{collections::HashMap, rc::Rc};
use uuid::Uuid;

pub struct Layout {
    pub keys: Vec<String>,
    pub messages: Rc<[usize]>,
    pub locked_history: bool,
    pub model: VariableHeightModel,
    pub generation: Uuid,
    pub follow: bool,
    stamp: String,
    used: std::time::Instant,
}
impl Default for Layout {
    fn default() -> Self {
        Self {
            keys: Vec::new(),
            messages: Rc::from([]),
            locked_history: false,
            model: VariableHeightModel::new([]),
            generation: Uuid::nil(),
            follow: true,
            stamp: String::new(),
            used: std::time::Instant::now(),
        }
    }
}
pub fn scroll_to(key: &RoomKey, y: f32) -> Task<Message> {
    iced::advanced::widget::operate(iced::advanced::widget::operation::scrollable::scroll_to(
        super::history::transcript_id(key),
        AbsoluteOffset {
            x: None,
            y: Some(y),
        },
        Animation::Instant,
    ))
}
impl Message {
    pub(super) fn changes_transcript_content(&self) -> bool {
        match self {
            Self::WorkCompleted(_, message) | Self::SessionResult(_, message) => {
                message.changes_transcript_content()
            }
            Self::WorkChanged
            | Self::Editor(_)
            | Self::InsertEmoji(_)
            | Self::ComposerMeasured(..)
            | Self::ComposerScrollBoundary
            | Self::TranscriptMeasured(..)
            | Self::TranscriptViewport(..)
            | Self::TranscriptScrolled(..)
            | Self::SidebarScrolled(_)
            | Self::PreviewVisible(_)
            | Self::PreviewHidden(_)
            | Self::PreviewReady(..)
            | Self::AvatarVisibility(..)
            | Self::AvatarReady(..)
            | Self::AvatarCacheSaved(_)
            | Self::PresenceFinished
            | Self::PresenceReadFailed(..) => false,
            _ => true,
        }
    }
}
impl App {
    pub fn locked_history(&self, room: &messages_core::Conversation) -> bool {
        self.scroll_positions
            .get(&room.key)
            .map_or_else(|| room.has_locked_history(), |p| p.layout.locked_history)
    }

    pub(super) fn prepare_transcript(&mut self, restore: bool) -> Task<Message> {
        self.maintain_transcript(restore, true)
    }

    pub(super) fn maintain_transcript(&mut self, restore: bool, reconcile: bool) -> Task<Message> {
        let Some(key) = self.selected.clone() else {
            return Task::none();
        };
        let Some(room) = self.store.room(&key) else {
            return Task::none();
        };
        let state = self.scroll_positions.entry(key.clone()).or_default();
        let reconcile = reconcile || state.layout.keys.is_empty();
        if reconcile {
            state.layout.locked_history = room.has_locked_history();
            if self.search_hit.as_ref().is_some_and(|id| {
                room.messages
                    .iter()
                    .any(|message| &message.id == id && message.waiting_for_keys())
            }) {
                self.search_hit = None;
            }
        }
        let stamp = format!(
            "{}:{}:{}:{}:{}:{:?}",
            self.window_size.width,
            self.preferences.text_size,
            self.preferences.bubble_radius,
            self.preferences.group_messages,
            state.layout.locked_history,
            self.themes.theme().typography
        );
        if state.viewport_height <= 0.0 {
            state.viewport_height = self.window_size.height.max(1.0);
        }
        let viewport = state.viewport_height;
        state.layout.used = std::time::Instant::now();
        let same_keys = if reconcile {
            state.layout.messages = room
                .messages
                .iter()
                .enumerate()
                .filter(|(_, message)| message.is_readable())
                .map(|(i, _)| i)
                .collect();
            state.layout.keys.len() == state.layout.messages.len() + 2
                && state
                    .layout
                    .messages
                    .iter()
                    .map(|&i| &room.messages[i])
                    .zip(state.layout.keys.iter().skip(1))
                    .all(|(m, k)| k.strip_prefix("m:") == Some(m.id.as_str()))
        } else {
            true
        };
        let changed = !same_keys || state.layout.stamp != stamp;
        if changed {
            let anchor = state
                .layout
                .model
                .index_at(state.y)
                .map(|i| {
                    if state.layout.keys.len() > 2 {
                        i.max(1)
                    } else {
                        i
                    }
                })
                .and_then(|i| {
                    state
                        .layout
                        .keys
                        .get(i)
                        .map(|k| (k.clone(), state.y - state.layout.model.offset_of(i)))
                });
            // Retain estimates across resizing: unchanged intrinsic heights do not re-emit measurements.
            let old: HashMap<_, _> = state
                .layout
                .keys
                .iter()
                .enumerate()
                .map(|(i, k)| (k.clone(), state.layout.model.height_of(i)))
                .collect();
            state.layout.keys = std::iter::once("header".into())
                .chain(
                    state
                        .layout
                        .messages
                        .iter()
                        .map(|&i| format!("m:{}", room.messages[i].id)),
                )
                .chain(std::iter::once("footer".into()))
                .collect();
            state.layout.model = VariableHeightModel::new(
                state
                    .layout
                    .keys
                    .iter()
                    .map(|k| old.get(k).copied().unwrap_or(72.0)),
            );
            if !state.layout.follow
                && let Some((id, within)) = anchor
                && let Some(i) = state.layout.keys.iter().position(|k| *k == id)
            {
                state.y = state.layout.model.offset_of(i) + within;
            }
            state.layout.generation = Uuid::new_v4();
            state.layout.stamp = stamp;
        }
        state.viewport_height = viewport;
        state.height = state.layout.model.total_height();
        if state.layout.follow {
            state.y = (state.height - viewport).max(0.0);
        }
        state.y = state.y.clamp(0.0, (state.height - viewport).max(0.0));
        let window = state.layout.model.range_for(state.y, state.viewport_height);
        let start = window
            .start
            .saturating_sub(5)
            .min(state.layout.messages.len());
        let end = (window.end + 3)
            .saturating_sub(1)
            .min(state.layout.messages.len());
        let visible_ids: std::collections::HashSet<_> = state.layout.messages[start..end]
            .iter()
            .flat_map(|&i| room.messages[i].attachments.iter().map(|f| f.id.as_str()))
            .collect();
        self.visible_previews
            .retain(|p| p.room == key && visible_ids.contains(p.id.as_str()));
        let task = if changed || restore {
            scroll_to(&key, state.y)
        } else {
            Task::none()
        };
        while self.scroll_positions.len() > 8 {
            let oldest = self
                .scroll_positions
                .iter()
                .filter(|(k, _)| **k != key)
                .min_by_key(|(_, p)| p.layout.used)
                .map(|(k, _)| k.clone());
            if let Some(oldest) = oldest {
                self.scroll_positions.remove(&oldest);
            } else {
                break;
            }
        }
        task
    }
    pub(super) fn transcript_viewport(&mut self, key: RoomKey, size: iced::Size) -> Task<Message> {
        if self.selected.as_ref() != Some(&key) || !size.height.is_finite() || size.height <= 0.0 {
            return Task::none();
        }
        let Some(p) = self.scroll_positions.get_mut(&key) else {
            return Task::none();
        };
        if (p.viewport_height - size.height).abs() <= 0.5 {
            return Task::none();
        }
        p.viewport_height = size.height;
        p.y = if p.layout.follow {
            (p.layout.model.total_height() - size.height).max(0.0)
        } else {
            p.y.min((p.layout.model.total_height() - size.height).max(0.0))
        };
        scroll_to(&key, p.y)
    }
    pub(super) fn measure_transcript(
        &mut self,
        key: RoomKey,
        generation: Uuid,
        row: &str,
        measurement: RowMeasurement,
    ) -> Task<Message> {
        if self.selected.as_ref() != Some(&key) {
            return Task::none();
        }
        let Some(p) = self.scroll_positions.get_mut(&key) else {
            return Task::none();
        };
        if !measurement.height.is_finite() || measurement.height <= 0.0 {
            return Task::none();
        }
        // The row measurer reports a height once and does not repeat it while
        // the height stays the same, so a report rendered under an earlier
        // layout generation must still land: it is placed by the row's key,
        // not by its index, which a regenerated layout may have moved.
        let index = if p.layout.generation == generation
            && p.layout.keys.get(measurement.index).map(String::as_str) == Some(row)
        {
            measurement.index
        } else if let Some(index) = p.layout.keys.iter().position(|k| k == row) {
            index
        } else {
            return Task::none();
        };
        let measurement = RowMeasurement {
            index,
            height: measurement.height,
        };
        if (p.layout.model.height_of(measurement.index) - measurement.height).abs() <= 0.5 {
            return Task::none();
        }
        p.y = enzui_iced_widgets::virtual_list::reconcile_measurements(
            &mut p.layout.model,
            p.y,
            [measurement.into()],
        );
        p.height = p.layout.model.total_height();
        if p.layout.follow {
            p.y = (p.height - p.viewport_height).max(0.0);
        }
        scroll_to(&key, p.y)
    }
}
