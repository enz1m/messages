use super::{App, Message, jobs};
use enzui_iced_core::theme_file::{ModePreference, ThemeDocument, ThemePaths};
use iced::Task;
use std::{
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy)]
pub(super) struct Change {
    pub mode: Option<bool>,
    pub migrate: bool,
}

#[derive(Default)]
pub(super) struct Writer {
    pub path: Option<PathBuf>,
    pub active: bool,
    pub pending: Option<Change>,
    pub last_load: u64,
    pub chosen: bool,
}

impl Writer {
    pub fn from_identity() -> Self {
        Self {
            path: enzui_iced_core::app_identity::declared()
                .and_then(|app| ThemePaths::from_env().app_user_path(&app)),
            ..Self::default()
        }
    }
}

impl App {
    pub(super) fn queue_theme(&mut self, mode: Option<bool>, migrate: bool) -> Task<Message> {
        self.theme_writer.chosen |= !migrate;
        self.theme_writer.pending = Some(Change { mode, migrate });
        self.pump_theme()
    }

    pub(super) fn pump_theme(&mut self) -> Task<Message> {
        if self.theme_writer.active {
            return Task::none();
        }
        let Some(change) = self.theme_writer.pending.take() else {
            return Task::none();
        };
        let Some(path) = self.theme_writer.path.clone() else {
            self.notice = Some("The application theme location is unavailable.".into());
            return Task::none();
        };
        self.theme_writer.active = true;
        self.track_job(
            jobs::Kind::Persistence,
            async move {
                tokio::task::spawn_blocking(move || save_mode(&path, change))
                    .await
                    .map_err(|_| "The application theme could not be saved.".to_owned())?
            },
            Message::ThemeSaved,
        )
    }

    pub(super) fn theme_saved(&mut self, result: Result<(), String>) -> Task<Message> {
        self.theme_writer.active = false;
        if let Err(error) = &result {
            self.notice = Some(error.clone().into());
        }
        if self.theme_writer.pending.is_some() {
            return self.pump_theme();
        }
        if result.is_ok() {
            self.themes.set_mode_override(None);
        }
        self.closing
            .map(|id| self.request_close(id))
            .unwrap_or_else(Task::none)
    }
}

pub(super) fn save_mode(path: &Path, change: Change) -> Result<(), String> {
    let mut document = match enzui_iced_core::atomic_state::open_state_file_nofollow(path) {
        Ok(file) => {
            if change.migrate {
                return Ok(());
            }
            let mut text = String::new();
            file.take(enzui_iced_core::theme_file::MAX_THEME_FILE_BYTES + 1)
                .read_to_string(&mut text)
                .map_err(|_| "The application theme could not be read.")?;
            if text.len() as u64 > enzui_iced_core::theme_file::MAX_THEME_FILE_BYTES {
                return Err("The application theme is too large; it was preserved.".into());
            }
            ThemeDocument::parse(&text)
                .map_err(|_| "The application theme is invalid; it was preserved.")?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ThemeDocument::empty(),
        Err(_) => {
            return Err("The application theme could not be opened; it was preserved.".into());
        }
    };
    document.set_mode_preference(match change.mode {
        Some(true) => ModePreference::Dark,
        Some(false) => ModePreference::Light,
        None => ModePreference::System,
    });
    enzui_iced_core::theme_file::save_theme_atomic(path, &document.to_toml())
        .map_err(|_| "The application theme could not be saved.".into())
}
