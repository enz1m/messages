use super::{Dialog, Message};
use enzui_iced_core::localization::text;
use enzui_iced_core::shortcut::{Modifier, NamedKey, Shortcut};
use enzui_iced_core::{ShortcutBinding, action::KeyRouteScope};

pub fn catalog() -> Vec<ShortcutBinding<Message, KeyRouteScope>> {
    [
        (
            "messages-search",
            "messages-search",
            Shortcut::new(Modifier::CTRL, 'f'),
            Message::FocusSearch,
        ),
        (
            "messages-new",
            "messages-new-conversation",
            Shortcut::new(Modifier::CTRL, 'n'),
            Message::Open(Dialog::NewConversation),
        ),
        (
            "messages-settings",
            "messages-settings",
            Shortcut::new(Modifier::CTRL, ','),
            Message::Open(Dialog::Settings),
        ),
        (
            "messages-help",
            "messages-shortcuts",
            Shortcut::new(Modifier::NONE, NamedKey::F1),
            Message::ShowShortcuts,
        ),
    ]
    .into_iter()
    .map(|(id, label, chord, message)| {
        ShortcutBinding::new(id, message, chord)
            .with_description(text(label))
            .when(KeyRouteScope::is_global)
    })
    .collect()
}

impl super::App {
    pub(super) fn shortcut_settings(
        &mut self,
        event: enzui_iced_widgets::editable_shortcuts::EditableShortcutsEvent,
    ) {
        use enzui_iced_core::persist::Persistable;
        use enzui_iced_core::{Modifier, ShortcutValidity};
        use enzui_iced_widgets::{
            editable_shortcuts::EditableShortcutsEvent as E, key_capture_chip::KeyCapture,
        };
        self.shortcut_feedback = None;
        match event {
            E::Capture {
                action,
                event: KeyCapture::Started,
            } => {
                self.shortcut_armed = Some(action);
                return;
            }
            E::Capture {
                action,
                event: KeyCapture::Candidate(chord),
            } => {
                if self.shortcut_armed.as_deref() != Some(action.as_str()) {
                    return;
                }
                if let Err(error) = self.shortcuts.try_rebind(&action, chord, |chord| {
                    if chord.modifiers() == Modifier::NONE && chord.key().as_named().is_none() {
                        ShortcutValidity::Invalid("Use a modifier with a typing key.".into())
                    } else {
                        ShortcutValidity::Valid
                    }
                }) {
                    self.shortcut_feedback = Some((action, error.message()));
                }
            }
            E::Capture {
                action,
                event: KeyCapture::Cleared,
            } => {
                self.shortcut_feedback = Some((
                    action,
                    "Choose a chord, or use Reset to restore the default.".into(),
                ));
            }
            E::Capture {
                event: KeyCapture::Cancelled,
                ..
            } => {}
            E::Reset { action } => {
                self.shortcuts.reset(&action);
            }
            E::ResetAll => {
                self.shortcuts.reset_all();
            }
        }
        self.shortcut_armed = None;
        match self.shortcuts.overrides().persist_serialize() {
            Ok(body) => {
                self.preferences.shortcut_overrides = body;
                self.changed();
            }
            Err(error) => self.shortcut_feedback = Some(("messages-search".into(), error)),
        }
    }

    pub(super) fn restore_shortcuts(&mut self) {
        self.emoji_recents = enzui_iced_widgets::emoji_picker::EmojiRecents::from_list(
            self.preferences.emoji_recents.clone(),
        );
        use enzui_iced_core::persist::Persistable;
        let overrides = enzui_iced_core::ShortcutOverrides::persist_deserialize(
            &self.preferences.shortcut_overrides,
        )
        .unwrap_or_default();
        self.shortcuts =
            enzui_iced_core::EditableShortcutCatalog::from_overrides(catalog(), overrides);
    }
}
