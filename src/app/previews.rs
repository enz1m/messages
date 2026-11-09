use std::num::NonZeroUsize;

use crate::platform::previews::Thumbnail;
use enzui_iced_core::blocking_task::BlockingTaskOwner;
use iced::Task;
use messages_core::{Attachment, RoomKey};

use super::{App, Message, lifecycle::Session};

const CACHE_BYTES: u64 = 32 * 1024 * 1024;
const CACHE_ENTRIES: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct PreviewKey {
    pub room: RoomKey,
    pub id: String,
}

pub enum Preview {
    Loading,
    Ready(Thumbnail),
    Unavailable,
}

pub fn worker_pool() -> BlockingTaskOwner {
    let workers = NonZeroUsize::new(2).expect("two workers are nonzero");
    let queue = NonZeroUsize::new(4).expect("four queue slots are nonzero");
    BlockingTaskOwner::new("messages-preview", workers, queue)
        .unwrap_or_else(|_| BlockingTaskOwner::unavailable(queue))
}

impl App {
    pub fn image_preview(&self, room: &RoomKey, id: &str) -> Option<&Thumbnail> {
        match self.previews.get(&PreviewKey {
            room: room.clone(),
            id: id.into(),
        }) {
            Some(Preview::Ready(asset)) => Some(asset),
            _ => None,
        }
    }

    pub(super) fn load_previews(&mut self) -> Task<Message> {
        if self.stopping() {
            return Task::none();
        }
        if self.loading || (self.window_size.width < 760.0 && !self.show_detail) {
            return Task::none();
        }
        let available = 2usize.saturating_sub(self.preview_tasks.len());
        if available == 0 {
            return Task::none();
        }
        if self
            .selected
            .as_ref()
            .is_some_and(|key| !self.scroll_positions.contains_key(key))
        {
            let _ = self.prepare_transcript(false);
        }
        let Some(room) = self.current_room() else {
            return Task::none();
        };
        let key = room.key.clone();
        let mut files: Vec<_> = room
            .draft_attachments
            .iter()
            .filter(|file| file.mime.starts_with("image/"))
            .map(|file| Attachment {
                id: format!("local:{}", file.id),
                name: file.name.clone(),
                mime: file.mime.clone(),
                bytes: file.bytes,
                remote_url: Some(file.path.to_string_lossy().into()),
            })
            .collect();
        if let Some(p) = self.scroll_positions.get(&key) {
            let visible = p.layout.model.range_for(p.y, p.viewport_height);
            let length = p.layout.messages.len();
            let range = visible.start.saturating_sub(5).min(length)
                ..(visible.end + 3).saturating_sub(1).min(length);
            files.extend(
                p.layout.messages[range]
                    .iter()
                    .rev()
                    .filter_map(|&i| room.messages.get(i))
                    .filter(|message| !message.deleted)
                    .flat_map(|message| message.attachments.iter())
                    .filter(|file| {
                        file.mime.starts_with("image/")
                            && (file.id.starts_with("local:")
                                || self.adapters.contains_key(&key.account))
                            && self.visible_previews.contains(&PreviewKey {
                                room: key.clone(),
                                id: file.id.clone(),
                            })
                    })
                    .cloned(),
            );
        }
        let files: Vec<_> = files
            .into_iter()
            .filter(|file| {
                !self.previews.contains_key(&PreviewKey {
                    room: key.clone(),
                    id: file.id.clone(),
                })
            })
            .take(available)
            .collect();
        let mut tasks = Vec::new();
        for file in files {
            let preview = PreviewKey {
                room: key.clone(),
                id: file.id.clone(),
            };
            let session = self.session(key.account);
            let root = self.file_root(&key).join("outgoing");
            let room = key.clone();
            let adapter = self.adapters.get(&key.account).cloned();
            let dispatcher = self.preview_pool.dispatcher();
            self.previews.insert(preview.clone(), Preview::Loading);
            let completion = preview.clone();
            let (task, handle) = self
                .track_job(
                    super::jobs::Kind::Preview,
                    async move {
                        let bytes = if file.id.starts_with("local:") {
                            crate::platform::files::read_private(
                                root,
                                file.remote_url.as_deref().ok_or("Image unavailable")?,
                            )
                            .await?
                        } else {
                            adapter
                                .ok_or("Reconnect to preview this image")?
                                .attachment(&room, &file)
                                .await
                                .map_err(|error| error.to_string())?
                        };
                        dispatcher
                            .submit_waiting(0, move |context| {
                                crate::platform::previews::decode(&bytes, || context.is_cancelled())
                            })
                            .await
                            .into_result()
                            .map_err(|error| error.to_string())?
                    },
                    move |result| Message::PreviewReady(session, completion.clone(), result),
                )
                .abortable();
            self.preview_tasks.insert(preview, handle.abort_on_drop());
            tasks.push(task);
        }
        Task::batch(tasks)
    }

    pub(super) fn preview_ready(
        &mut self,
        session: Session,
        key: PreviewKey,
        result: Result<Thumbnail, String>,
    ) -> Task<Message> {
        if !self.session_is_current(session) || session.account != key.room.account {
            return Task::none();
        }
        self.preview_tasks.remove(&key);
        if self.store.room(&key.room).is_none() {
            self.previews.remove(&key);
            return Task::none();
        }
        self.previews.insert(
            key,
            match result {
                Ok(asset) => Preview::Ready(asset),
                Err(_) => Preview::Unavailable,
            },
        );
        self.trim_previews();
        self.load_previews()
    }

    fn trim_previews(&mut self) {
        let mut bytes: u64 = self
            .previews
            .values()
            .map(|value| match value {
                Preview::Ready(asset) => asset.rgba_bytes,
                _ => 0,
            })
            .sum();
        let mut keys: Vec<_> = self
            .previews
            .keys()
            .filter(|key| !self.preview_tasks.contains_key(*key))
            .cloned()
            .collect();
        keys.sort_by_key(|key| {
            self.visible_previews.contains(key)
                || self.store.room(&key.room).is_some_and(|room| {
                    room.draft_attachments
                        .iter()
                        .any(|file| key.id == format!("local:{}", file.id))
                })
        });
        for key in keys {
            if bytes <= CACHE_BYTES && self.previews.len() <= CACHE_ENTRIES {
                break;
            }
            if let Some(Preview::Ready(asset)) = self.previews.remove(&key) {
                bytes = bytes.saturating_sub(asset.rgba_bytes);
            }
        }
    }
}
