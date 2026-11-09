use base64::{Engine, engine::general_purpose::STANDARD};
use iced::widget::image::Handle;
use messages_core::{MessageStore, RoomKey};
use serde::{Deserialize, Serialize};
use std::{io::Read, path::PathBuf};

const MAX_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_ENTRIES: usize = 64;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Photo {
    pub key: RoomKey,
    pub revision: u64,
    owner: String,
    pub saved: u64,
    width: u32,
    height: u32,
    rgba: String,
}
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
impl Photo {
    pub fn new(key: RoomKey, revision: u64, owner: String, handle: &Handle) -> Option<Self> {
        let Handle::Rgba {
            width,
            height,
            pixels,
            ..
        } = handle
        else {
            return None;
        };
        if *width == 0 || *height == 0 || *width > 128 || *height > 128 {
            return None;
        }
        Some(Self {
            key,
            revision,
            owner,
            saved: now(),
            width: *width,
            height: *height,
            rgba: STANDARD.encode(pixels.as_ref()),
        })
    }
    pub fn belongs_to(&self, store: &MessageStore) -> bool {
        store
            .accounts
            .iter()
            .any(|a| a.id == self.key.account && a.user_id == self.owner)
            && store
                .room(&self.key)
                .is_some_and(|r| r.avatar_revision == self.revision)
    }
    pub fn handle(&self) -> Option<Handle> {
        if self.width == 0
            || self.height == 0
            || self.width > 128
            || self.height > 128
            || self.rgba.len() > 128 * 128 * 4 * 2
        {
            return None;
        }
        let bytes = STANDARD.decode(&self.rgba).ok()?;
        (bytes.len() == (self.width * self.height * 4) as usize)
            .then(|| Handle::from_rgba(self.width, self.height, bytes))
    }
}
fn path(workspace: PathBuf) -> PathBuf {
    workspace.with_file_name("avatar-cache.json")
}
pub async fn load(workspace: PathBuf, store: &MessageStore) -> Vec<Photo> {
    let bytes = tokio::task::spawn_blocking(move || {
        let file =
            enzui_iced_core::atomic_state::open_state_file_nofollow(&path(workspace)).ok()?;
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1).read_to_end(&mut bytes).ok()?;
        (bytes.len() as u64 <= MAX_BYTES).then_some(bytes)
    })
    .await
    .ok()
    .flatten();
    let Some(bytes) = bytes else {
        return Vec::new();
    };
    let Ok(mut photos) = serde_json::from_slice::<Vec<Photo>>(&bytes) else {
        return Vec::new();
    };
    photos.retain(|p| {
        p.saved <= now() && now().saturating_sub(p.saved) < 86400 && p.belongs_to(store)
    });
    photos.sort_by_key(|p| std::cmp::Reverse(p.saved));
    photos.truncate(MAX_ENTRIES);
    photos
}
pub async fn save(workspace: PathBuf, photos: Vec<Photo>) {
    let _ = tokio::task::spawn_blocking(move || {
        let bytes = serde_json::to_vec(&photos).ok()?;
        if bytes.len() as u64 > MAX_BYTES {
            return None;
        }
        enzui_iced_core::atomic_state::save_atomic_raw(&path(workspace), &bytes).ok()
    })
    .await;
}
