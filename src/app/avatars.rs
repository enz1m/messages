use super::{App, Message, jobs, lifecycle::Session};
use iced::{Task, widget::image::Handle};
use messages_core::RoomKey;
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

#[derive(Default)]
pub struct Avatars {
    entries: HashMap<RoomKey, Entry>,
    visible: std::collections::HashSet<RoomKey>,
    pending: HashMap<RoomKey, iced::task::Handle>,
    cached: HashMap<RoomKey, Cached>,
    revision: u64,
    saved: u64,
    saving: bool,
}
struct Cached {
    photo: crate::platform::avatar_cache::Photo,
    image: Handle,
    live_session: Option<Session>,
}
struct Entry {
    revision: u64,
    session: Session,
    time: Instant,
    ttl: Duration,
    image: Option<Handle>,
}
impl App {
    pub fn avatar_image(&self, key: &RoomKey) -> Option<Handle> {
        let revision = self.store.room(key)?.avatar_revision;
        self.avatars
            .entries
            .get(key)
            .filter(|e| self.session_is_current(e.session) && e.revision == revision)
            .and_then(|e| e.image.clone())
            .or_else(|| {
                self.avatars
                    .cached
                    .get(key)
                    .filter(|c| {
                        c.live_session
                            .is_none_or(|owner| owner == self.session(key.account))
                            && c.photo.revision == revision
                            && c.photo.belongs_to(&self.store)
                            && crate::platform::avatar_cache::now().saturating_sub(c.photo.saved)
                                < 86400
                    })
                    .map(|c| c.image.clone())
            })
    }
    pub(super) fn save_avatars(&mut self) -> Task<Message> {
        if self.loading || self.avatars.saving || self.avatars.saved == self.avatars.revision {
            return Task::none();
        }
        self.avatars
            .cached
            .retain(|_, c| c.photo.belongs_to(&self.store));
        self.avatars.saving = true;
        let revision = self.avatars.revision;
        let photos = self
            .avatars
            .cached
            .values()
            .map(|c| c.photo.clone())
            .collect();
        self.track_job(
            jobs::Kind::Persistence,
            crate::platform::avatar_cache::save(self.storage_path.clone(), photos),
            move |_| Message::AvatarCacheSaved(revision),
        )
    }
    pub(super) fn load_avatars(&mut self) -> Task<Message> {
        if self.loading || self.stopping() {
            return Task::none();
        }
        self.avatars
            .entries
            .retain(|key, _| self.store.room(key).is_some());
        self.avatars
            .visible
            .retain(|key| self.store.room(key).is_some());
        let mut keys: Vec<_> = self
            .visible_rooms()
            .into_iter()
            .filter(|r| self.avatars.visible.contains(&r.key))
            .take(64)
            .map(|r| r.key.clone())
            .collect();
        if let Some(key) = self.selected.clone() {
            keys.retain(|k| *k != key);
            keys.insert(0, key);
        }
        keys.truncate(64);
        let mut tasks = Vec::new();
        for key in keys {
            if self.avatars.pending.len() >= 2 {
                break;
            }
            let Some(adapter) = self
                .adapters
                .get(&key.account)
                .filter(|a| a.supports_avatars())
                .cloned()
            else {
                continue;
            };
            let session = self.session(key.account);
            let revision = self
                .store
                .room(&key)
                .map(|r| r.avatar_revision)
                .unwrap_or_default();
            if self.avatars.cached.get(&key).is_some_and(|c| {
                c.live_session.is_none_or(|owner| owner == session)
                    && c.photo.revision == revision
                    && c.photo.belongs_to(&self.store)
                    && crate::platform::avatar_cache::now().saturating_sub(c.photo.saved) < 600
            }) {
                continue;
            }
            if self.avatars.pending.contains_key(&key)
                || self.avatars.entries.get(&key).is_some_and(|e| {
                    e.revision == revision && e.session == session && e.time.elapsed() < e.ttl
                })
            {
                continue;
            }
            if self.avatars.entries.len() >= 64 {
                let oldest = self
                    .avatars
                    .entries
                    .iter()
                    .min_by_key(|(_, e)| e.time)
                    .map(|(k, _)| k.clone());
                if let Some(oldest) = oldest {
                    self.avatars.entries.remove(&oldest);
                }
            }
            let pending_key = key.clone();
            let target = key.clone();
            let dispatcher = self.preview_pool.dispatcher();
            let (task, handle) = self
                .track_job(
                    jobs::Kind::Preview,
                    async move {
                        let Some(bytes) = adapter.avatar(&target).await.map_err(|_| ())? else {
                            return Ok(None);
                        };
                        let asset = dispatcher
                            .submit_waiting(0, move |context| {
                                crate::platform::previews::decode_avatar(&bytes, || {
                                    context.is_cancelled()
                                })
                            })
                            .await
                            .into_result()
                            .map_err(|_| ())?
                            .map_err(|_| ())?;
                        Ok(Some(asset.handle))
                    },
                    move |result| Message::AvatarReady(session, key.clone(), revision, result),
                )
                .abortable();
            self.avatars
                .pending
                .insert(pending_key, handle.abort_on_drop());
            tasks.push(task);
        }
        Task::batch(tasks)
    }
    pub(super) fn avatar_ready(
        &mut self,
        session: Session,
        key: RoomKey,
        revision: u64,
        result: Result<Option<Handle>, ()>,
    ) -> Task<Message> {
        if !self.session_is_current(session) || session.account != key.account {
            return Task::none();
        }
        self.avatars.pending.remove(&key);
        if self
            .store
            .room(&key)
            .is_none_or(|r| r.avatar_revision != revision)
        {
            return Task::none();
        }
        let image = match &result {
            Ok(Some(image)) => {
                if let Some(photo) = crate::platform::avatar_cache::Photo::new(
                    key.clone(),
                    revision,
                    self.store
                        .accounts
                        .iter()
                        .find(|a| a.id == key.account)
                        .map(|a| a.user_id.clone())
                        .unwrap_or_default(),
                    image,
                ) {
                    self.avatars.cached.insert(
                        key.clone(),
                        Cached {
                            photo,
                            image: image.clone(),
                            live_session: Some(session),
                        },
                    );
                    self.avatars.revision += 1;
                }
                Some(image.clone())
            }
            Ok(None) => {
                if self.avatars.cached.remove(&key).is_some() {
                    self.avatars.revision += 1;
                }
                None
            }
            Err(()) => self.avatar_image(&key),
        };
        while self.avatars.cached.len() > crate::platform::avatar_cache::MAX_ENTRIES {
            let oldest = self
                .avatars
                .cached
                .iter()
                .min_by_key(|(_, c)| c.photo.saved)
                .map(|(k, _)| k.clone())
                .unwrap();
            self.avatars.cached.remove(&oldest);
        }
        self.avatars.entries.insert(
            key,
            Entry {
                revision,
                session,
                time: Instant::now(),
                ttl: Duration::from_secs(if result.is_ok() { 600 } else { 60 }),
                image,
            },
        );
        while self.avatars.entries.len() > 64 {
            let oldest = self
                .avatars
                .entries
                .iter()
                .min_by_key(|(_, e)| e.time)
                .map(|(key, _)| key.clone())
                .unwrap();
            self.avatars.entries.remove(&oldest);
        }
        Task::none()
    }
    pub(super) fn avatar_visibility(
        &mut self,
        session: Session,
        key: RoomKey,
        shown: bool,
    ) -> Task<Message> {
        if !self.session_is_current(session) || session.account != key.account {
            return Task::none();
        }
        if shown {
            if self.avatars.visible.len() < 128 {
                self.avatars.visible.insert(key);
            }
        } else {
            self.avatars.visible.remove(&key);
        }
        self.load_avatars()
    }
    pub(super) fn retire_avatars(&mut self, account: messages_core::AccountId) {
        self.avatars.entries.retain(|key, _| key.account != account);
        self.avatars.pending.retain(|key, _| key.account != account);
        self.avatars.visible.retain(|key| key.account != account);
    }
}

impl Avatars {
    pub(super) fn restore(&mut self, photos: Vec<crate::platform::avatar_cache::Photo>) {
        self.cached = photos
            .into_iter()
            .filter_map(|p| {
                p.handle().map(|h| {
                    (
                        p.key.clone(),
                        Cached {
                            photo: p,
                            image: h,
                            live_session: None,
                        },
                    )
                })
            })
            .collect();
    }
    pub(super) fn saved(&mut self, revision: u64) {
        self.saving = false;
        self.saved = revision;
    }
    pub(super) fn forget(&mut self, account: messages_core::AccountId) {
        let before = self.cached.len();
        self.cached.retain(|key, _| key.account != account);
        if self.cached.len() != before {
            self.revision += 1;
        }
    }
}
