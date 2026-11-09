use std::path::PathBuf;

use messages_core::MessageStore;
use serde::{Deserialize, Serialize};
use std::io::Read;
mod encryption;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: u32,
    #[serde(default)]
    pub preferences: Preferences,
    pub store: MessageStore,
    pub demo: bool,
    #[serde(default)]
    pub selected: Option<messages_core::RoomKey>,
    #[serde(default)]
    pub dark: Option<bool>,
    pub connections: Vec<messages_protocols::ConnectionConfig>,
    #[serde(default)]
    pub credential_keys: std::collections::BTreeMap<uuid::Uuid, uuid::Uuid>,
    #[serde(default)]
    pub credential_cleanup: Vec<crate::platform::credentials::CredentialRef>,
    /// Removed accounts whose helper storage and files may still be on disk.
    #[serde(default)]
    pub account_purges: Vec<uuid::Uuid>,
}

pub fn state_path() -> PathBuf {
    if let Some(path) = std::env::var_os("MESSAGES_STATE_DIR") {
        return PathBuf::from(path).join("state.json");
    }
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share")
        });
    base.join("enz-messages/state.json")
}

pub fn sample_path() -> PathBuf {
    state_path().with_file_name("sample-state.json")
}

pub async fn load_workspace() -> Result<Option<Snapshot>, String> {
    let saved = load(state_path()).await?;
    if saved.as_ref().is_some_and(|snapshot| !snapshot.demo) {
        return Ok(saved);
    }
    let sample = load(sample_path()).await?;
    Ok(sample.or(saved))
}

pub async fn load(path: PathBuf) -> Result<Option<Snapshot>, String> {
    let directory = path
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .to_owned();
    let bytes = tokio::task::spawn_blocking(move || {
        let file = match enzui_iced_core::atomic_state::open_state_file_nofollow(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err("Saved conversations could not be opened.".to_owned()),
            Ok(file) => file,
        };
        let mut bytes = Vec::new();
        file.take(32 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "Saved conversations could not be read.".to_owned())?;
        if bytes.len() > 32 * 1024 * 1024 {
            return Err("The saved conversation file is too large to load.".to_owned());
        }
        Ok(Some(bytes))
    })
    .await
    .map_err(|_| "Saved conversations could not be read.".to_owned())??;
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    let mut snapshot: Snapshot = serde_json::from_slice(&bytes).map_err(
        |_| "Saved conversations could not be decoded. The original file has been preserved.",
    )?;
    if snapshot.version != 1 {
        return Err("This saved state uses a newer Messages version.".into());
    }
    let mut snapshot = tokio::task::spawn_blocking(move || {
        encryption::restore_legacy_flags(&mut snapshot, &directory);
        snapshot
    })
    .await
    .map_err(|_| "Saved conversations could not be decoded.".to_owned())?;
    snapshot.store.recover_interrupted_sends();
    for account in &mut snapshot.store.accounts {
        if account.connection != messages_core::ConnectionState::Demo {
            account.connection = messages_core::ConnectionState::Disconnected;
        }
    }
    Ok(Some(snapshot))
}

pub async fn save(path: PathBuf, snapshot: Snapshot) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let bytes =
            serde_json::to_vec(&snapshot).map_err(|_| "Conversations could not be encoded.")?;
        enzui_iced_core::atomic_state::save_atomic_raw(&path, &bytes)
            .map_err(|_| "The saved state could not be committed.".to_owned())
    })
    .await
    .map_err(|_| "The saved state could not be committed.".to_owned())?
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub read_receipts: bool,
    pub typing_indicators: bool,
    pub text_size: u8,
    pub bubble_color: String,
    pub received_bubble_color: String,
    pub emoji_recents: Vec<String>,
    pub media_devices: messages_media::Selection,
    pub bubble_radius: f32,
    pub shortcut_overrides: String,
    pub group_messages: bool,
    pub notifications: bool,
    pub notification_preview: bool,
    pub accounts: std::collections::BTreeMap<uuid::Uuid, AccountPreferences>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            read_receipts: false,
            typing_indicators: false,
            text_size: 13,
            bubble_color: "System accent".into(),
            received_bubble_color: "System surface".into(),
            emoji_recents: Vec::new(),
            media_devices: Default::default(),
            bubble_radius: 18.0,
            shortcut_overrides: String::new(),
            group_messages: true,
            notifications: true,
            notification_preview: true,
            accounts: Default::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AccountPreferences {
    pub read_receipts: bool,
    pub typing_indicators: bool,
    pub notifications: bool,
}
impl Default for AccountPreferences {
    fn default() -> Self {
        Self {
            read_receipts: false,
            typing_indicators: false,
            notifications: true,
        }
    }
}
impl Preferences {
    pub fn account(&self, id: uuid::Uuid) -> AccountPreferences {
        self.accounts
            .get(&id)
            .cloned()
            .unwrap_or(AccountPreferences {
                read_receipts: self.read_receipts,
                typing_indicators: self.typing_indicators,
                ..Default::default()
            })
    }
    pub fn account_mut(&mut self, id: uuid::Uuid) -> &mut AccountPreferences {
        let inherited = self.account(id);
        self.accounts.entry(id).or_insert(inherited)
    }
}

#[cfg(test)]
mod tests;
