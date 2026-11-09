use super::*;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use whatsapp_rust::{
    ProfilePictureLookup, ProfilePictureRequest, ProfilePictureTarget, ProfilePictureType,
};

#[derive(Default, Serialize, Deserialize)]
pub(super) struct Name {
    saved: Option<String>,
    push: Option<String>,
    #[serde(default)]
    history: Option<String>,
    #[serde(default)]
    saved_time: i64,
    #[serde(default)]
    saved_owner: String,
    #[serde(default)]
    phone: Option<String>,
}
impl Name {
    fn display(&self) -> Option<&String> {
        self.saved
            .as_ref()
            .or_else(|| {
                (self.saved_time == 0)
                    .then_some(self.history.as_ref())
                    .flatten()
            })
            .or(self.push.as_ref())
    }
}
pub(super) fn history_name(cache: &mut Cache, jid: &str, name: &str) {
    if let Some(name) =
        clean(name).filter(|n| n != jid && *n != fallback(jid) && n != "WhatsApp contact")
    {
        cache.whatsapp.names.entry(jid.into()).or_default().history = Some(name);
        cache.whatsapp.names_dirty = true;
    }
}
fn clean(name: &str) -> Option<String> {
    let name = name.trim();
    (!name.is_empty() && name.len() <= 256 && !name.chars().any(char::is_control))
        .then(|| name.to_owned())
}
pub(super) fn fallback(jid: &str) -> String {
    if let Some(number) = jid.strip_suffix("@s.whatsapp.net")
        && !number.is_empty()
        && number.bytes().all(|b| b.is_ascii_digit())
    {
        return format!("+{number}");
    }
    if jid.ends_with("@g.us") {
        "Group conversation".into()
    } else if let Some(id) = jid.strip_suffix("@lid")
        && !id.is_empty()
        && id.len() <= 32
        && id.bytes().all(|b| b.is_ascii_digit())
    {
        format!("WhatsApp ID {id}")
    } else {
        "WhatsApp contact".into()
    }
}
pub(super) fn push_name(cache: &mut Cache, jid: &str, name: &str) {
    if let Some(name) = clean(name)
        && jid.len() <= 512
        && name != jid
    {
        cache.whatsapp.names.entry(jid.into()).or_default().push = Some(name);
        cache.whatsapp.names_dirty = true;
        repair_room(cache, jid);
    }
}
pub(super) fn repair_room(cache: &mut Cache, jid: &str) {
    let phone = cache.whatsapp.names.get(jid).and_then(|n| n.phone.as_ref());
    let alias =
        phone.and_then(|phone| cache.whatsapp.names.get(&format!("{phone}@s.whatsapp.net")));
    let name = cache
        .whatsapp
        .names
        .get(jid)
        .and_then(|n| n.display())
        .filter(|n| n.as_str() != "WhatsApp contact")
        .or_else(|| {
            alias
                .and_then(|n| n.display())
                .filter(|n| n.as_str() != "WhatsApp contact")
        })
        .cloned();
    let fallback = phone
        .map(|p| format!("+{p}"))
        .unwrap_or_else(|| fallback(jid));
    let retired_name = name.is_none()
        && (cache
            .whatsapp
            .names
            .get(jid)
            .is_some_and(|n| n.saved_time > 0)
            || alias.is_some_and(|n| n.saved_time > 0));
    if let Some(room) = cache.store.room_mut(&RoomKey::new(uuid::Uuid::nil(), jid)) {
        if let Some(name) = name.filter(|_| room.kind == RoomKind::Direct) {
            room.title = name;
        } else if retired_name
            || room.title.trim().is_empty()
            || room.title == jid
            || room.title == "WhatsApp contact"
            || room.title.starts_with("WhatsApp ID ")
            || room
                .title
                .strip_prefix('+')
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        {
            room.title = fallback;
        }
        for message in &mut room.messages {
            if message.own {
                message.sender_name = "You".into();
            } else if room.kind == RoomKind::Direct {
                message.sender_name = room.title.clone();
            } else if let Some(name) = cache
                .whatsapp
                .names
                .get(&message.sender)
                .and_then(|n| n.display())
            {
                message.sender_name = name.clone();
            }
        }
    }
}
pub(super) fn repair(cache: &mut Cache) {
    let legacy: Vec<_> = cache
        .store
        .conversations
        .iter()
        .filter(|r| r.kind == RoomKind::Direct && !cache.whatsapp.names.contains_key(&r.key.room))
        .map(|r| {
            (
                r.key.room.clone(),
                r.title.clone(),
                r.messages
                    .iter()
                    .rev()
                    .find(|m| {
                        !m.own && clean(&m.sender_name).is_some() && m.sender_name != m.sender
                    })
                    .map(|m| m.sender_name.clone()),
            )
        })
        .collect();
    for (jid, title, push) in legacy {
        history_name(cache, &jid, &title);
        if let Some(push) = push {
            push_name(cache, &jid, &push);
        }
    }
    let keys: Vec<_> = cache
        .store
        .conversations
        .iter()
        .map(|r| r.key.room.clone())
        .collect();
    for jid in keys {
        repair_room(cache, &jid);
    }
}

fn saved(cache: &mut Cache, jid: &str, name: Option<String>, timestamp: i64) {
    let entry = cache.whatsapp.names.entry(jid.into()).or_default();
    if timestamp < entry.saved_time {
        return;
    }
    entry.saved = name;
    entry.saved_time = timestamp;
    cache.whatsapp.names_dirty = true;
    repair_room(cache, jid);
}
pub(super) fn phone_alias(cache: &mut Cache, jid: &str, phone: &str) {
    if !jid.ends_with("@lid")
        || phone.is_empty()
        || phone.len() > 15
        || !phone.bytes().all(|b| b.is_ascii_digit())
    {
        return;
    }
    let entry = cache.whatsapp.names.entry(jid.into()).or_default();
    if entry.phone.as_deref() != Some(phone) {
        entry.phone = Some(phone.into());
        cache.whatsapp.names_dirty = true;
    }
    repair_room(cache, jid);
}
pub(super) fn event(cache: &mut Cache, event: &Event) -> bool {
    match event {
        Event::ContactUpdate(update) => {
            let name = update
                .action
                .full_name
                .as_deref()
                .and_then(clean)
                .or_else(|| update.action.first_name.as_deref().and_then(clean));
            let timestamp = update
                .action_timestamp
                .unwrap_or(update.timestamp)
                .timestamp_millis();
            for jid in [
                Some(update.jid.to_string()),
                update.action.lid_jid.clone(),
                update.action.pn_jid.clone(),
            ]
            .into_iter()
            .flatten()
            {
                saved(cache, &jid, name.clone(), timestamp);
                if let Some(entry) = cache.whatsapp.names.get_mut(&jid)
                    && entry.saved_time == timestamp
                {
                    entry.saved_owner = update.jid.to_string();
                }
            }
        }
        Event::GroupUpdate(update) => {
            if let whatsapp_rust::wacore::stanza::groups::GroupNotificationAction::Subject {
                subject,
                ..
            } = update.action.as_ref()
                && let Some(name) = clean(subject)
                && let Some(room) = cache.store.room_mut(&RoomKey::new(
                    uuid::Uuid::nil(),
                    update.group_jid.to_string(),
                ))
            {
                room.title = name;
                cache.whatsapp.names_dirty = true;
            }
        }
        Event::ContactRemoved(update) => {
            let jid = update.jid.to_string();
            let owner = cache
                .whatsapp
                .names
                .get(&jid)
                .map(|n| n.saved_owner.clone())
                .unwrap_or_default();
            let mut keys: Vec<_> = cache
                .whatsapp
                .names
                .iter()
                .filter(|(_, n)| !owner.is_empty() && n.saved_owner == owner)
                .map(|(key, _)| key.clone())
                .collect();
            if !keys.contains(&jid) {
                keys.push(jid);
            }
            for jid in keys {
                saved(
                    cache,
                    &jid,
                    None,
                    update
                        .action_timestamp
                        .unwrap_or(update.timestamp)
                        .timestamp_millis(),
                );
            }
        }
        Event::PictureUpdate(update) => {
            if let Some(room) = cache
                .store
                .room_mut(&RoomKey::new(uuid::Uuid::nil(), update.jid.to_string()))
            {
                room.avatar_revision = room.avatar_revision.saturating_add(1);
                cache.whatsapp.names_dirty = true;
            }
        }
        Event::ContactUpdated(update) => {
            if let Some(room) = cache
                .store
                .room_mut(&RoomKey::new(uuid::Uuid::nil(), update.jid.to_string()))
            {
                room.avatar_revision = room.avatar_revision.saturating_add(1);
                cache.whatsapp.names_dirty = true;
            }
        }
        _ => return false,
    }
    true
}

fn photo_url(value: &str) -> anyhow::Result<url::Url> {
    let url = url::Url::parse(value)?;
    let host = url.host_str().unwrap_or("");
    anyhow::ensure!(
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.port_or_known_default() == Some(443)
            && url.fragment().is_none()
            && ["whatsapp.net", "fbcdn.net"]
                .iter()
                .any(|domain| host == *domain || host.ends_with(&format!(".{domain}"))),
        "Unsupported profile photo origin"
    );
    Ok(url)
}
impl WhatsApp {
    pub(super) async fn refresh_phone_numbers(&self) {
        let keys: Vec<_> = self
            .cache
            .lock()
            .await
            .store
            .conversations
            .iter()
            .filter(|r| r.kind == RoomKind::Direct && r.key.room.ends_with("@lid"))
            .map(|r| r.key.room.clone())
            .collect();
        if keys.is_empty() {
            return;
        }
        let start = self
            .identity_cursor
            .fetch_add(32, std::sync::atomic::Ordering::Relaxed)
            % keys.len();
        for key in keys.iter().cycle().skip(start).take(keys.len().min(32)) {
            let Ok(jid) = key.parse::<Jid>() else {
                continue;
            };
            // This SDK lookup reads only its account-local cache/database, without network discovery.
            if let Ok(Some(entry)) = self.client.get_lid_pn_entry(&jid).await {
                phone_alias(&mut *self.cache.lock().await, key, &entry.phone_number);
            }
        }
    }
    pub(super) async fn profile_photo(&self, chat: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let jid: Jid = chat.parse()?;
        let revision = self
            .cache
            .lock()
            .await
            .store
            .room(&RoomKey::new(uuid::Uuid::nil(), chat))
            .ok_or_else(|| anyhow::anyhow!("Unknown chat"))?
            .avatar_revision;
        let needs_contacts = !self.cache.lock().await.whatsapp.contacts_imported;
        let refresh_contacts = if needs_contacts {
            let mut last = self
                .contacts_requested
                .lock()
                .map_err(|_| anyhow::anyhow!("Profile refresh unavailable"))?;
            if last.is_none_or(|last| last.elapsed() >= Duration::from_secs(60)) {
                *last = Some(Instant::now());
                true
            } else {
                false
            }
        } else {
            false
        };
        if refresh_contacts {
            // Persisted app-state versions do not guarantee that the contact mirror is populated.
            if let Ok(Ok(report))=tokio::time::timeout(Duration::from_secs(10),self.client.resync_app_state([whatsapp_rust::wacore::appstate::patch_decode::WAPatchName::CriticalUnblockLow],whatsapp_rust::AppStateResyncMode::Snapshot)).await
                && report.all_synced() {
                let mut cache=self.cache.lock().await;cache.whatsapp.contacts_imported=true;cache.whatsapp.names_dirty=true;
            }
        }
        let target = if chat.ends_with("@g.us") {
            let missing = self
                .cache
                .lock()
                .await
                .store
                .room(&RoomKey::new(uuid::Uuid::nil(), chat))
                .is_some_and(|r| r.title == fallback(chat) || r.title == chat);
            if missing
                && let Ok(Ok(group)) = tokio::time::timeout(
                    Duration::from_secs(5),
                    self.client.groups().fetch_metadata(&jid),
                )
                .await
                && let Some(name) = group.subject.as_deref().and_then(clean)
            {
                let mut cache = self.cache.lock().await;
                if let Some(room) = cache.store.room_mut(&RoomKey::new(uuid::Uuid::nil(), chat)) {
                    room.title = name;
                    cache.whatsapp.names_dirty = true;
                }
            }
            ProfilePictureTarget::Group(&jid)
        } else if chat.ends_with("@s.whatsapp.net") || chat.ends_with("@lid") {
            ProfilePictureTarget::Contact(&jid)
        } else {
            return Ok(None);
        };
        let result = self
            .client
            .pictures()
            .lookup(
                ProfilePictureRequest::new(target, ProfilePictureType::Preview)
                    .timeout(Some(Duration::from_secs(5))),
            )
            .await?;
        let ProfilePictureLookup::Found(picture) = result else {
            return Ok(None);
        };
        let url = photo_url(&picture.url)?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(8))
            .build()?;
        let mut response = client.get(url).send().await?.error_for_status()?;
        anyhow::ensure!(
            response.content_length().is_none_or(|n| n <= 262144),
            "Photo exceeds limit"
        );
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            anyhow::ensure!(bytes.len() + chunk.len() <= 262144, "Photo exceeds limit");
            bytes.extend_from_slice(&chunk);
        }
        anyhow::ensure!(
            self.cache
                .lock()
                .await
                .store
                .room(&RoomKey::new(uuid::Uuid::nil(), chat))
                .is_some_and(|r| r.avatar_revision == revision),
            "Profile changed during lookup"
        );
        Ok(Some(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn photo_download_origins_are_https_cdn_only_and_never_accept_credentials_or_private_hosts() {
        for valid in [
            "https://pps.whatsapp.net/photo?token=opaque",
            "https://scontent.fna.fbcdn.net/photo",
        ] {
            assert!(photo_url(valid).is_ok());
        }
        for invalid in [
            "http://pps.whatsapp.net/photo",
            "https://pps.whatsapp.net.evil.test/photo",
            "https://127.0.0.1/photo",
            "https://user:secret@pps.whatsapp.net/photo",
            "https://pps.whatsapp.net:444/photo",
            "file:///tmp/photo",
        ] {
            assert!(photo_url(invalid).is_err());
        }
    }
}
