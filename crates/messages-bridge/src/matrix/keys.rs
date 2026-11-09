use super::Matrix;
use futures_util::StreamExt;
use matrix_sdk::ruma::{EventId, RoomId, serde::Raw};
use messages_core::RoomKey;
use std::collections::BTreeSet;

impl Matrix {
    pub(super) async fn receive_keys(
        &self,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        let Some(mut updates) = self.client.encryption().room_keys_received_stream().await else {
            anyhow::bail!("Matrix encryption is unavailable");
        };
        tokio::select! {
            _ = shutdown.changed() => return Ok(()),
            result = self.retry_decryption(None, true) => result?,
        }
        loop {
            let (rooms, refetch) = tokio::select! {
                _ = shutdown.changed() => return Ok(()),
                _ = self.decryption_needed.notified() => (None, false),
                update = updates.next() => match update {
                    Some(Ok(keys)) => (Some(keys.into_iter().map(|key|key.room_id.to_string()).collect()), true),
                    Some(Err(_)) => (None, true),
                    None => return Ok(()),
                }
            };
            tokio::select! {
                _ = shutdown.changed() => return Ok(()),
                result = self.retry_decryption(rooms.as_ref(), refetch) => result?,
            }
        }
    }

    async fn retry_decryption(
        &self,
        rooms: Option<&BTreeSet<String>>,
        refetch: bool,
    ) -> anyhow::Result<()> {
        let pending = self.cache.lock().await.matrix.pending_decryption();
        let own = self.client.user_id().map(|id| id.as_str()).unwrap_or("");
        for (room_id, id, ciphertext) in pending {
            if rooms.is_some_and(|rooms| !rooms.contains(&room_id)) {
                continue;
            }
            let Some(room) = RoomId::parse(&room_id)
                .ok()
                .and_then(|id| self.client.get_room(&id))
            else {
                continue;
            };
            let migrated = ciphertext.is_none();
            let event = if let Some(ciphertext) = ciphertext {
                let Ok(raw) = Raw::from_json_string(ciphertext.to_string()) else {
                    continue;
                };
                room.decrypt_event(&raw, None).await
            } else if refetch {
                let Ok(id) = EventId::parse(&id) else {
                    continue;
                };
                room.event(
                    &id,
                    Some(
                        matrix_sdk::config::RequestConfig::new()
                            .retry_limit(0)
                            .timeout(std::time::Duration::from_secs(10)),
                    ),
                )
                .await
            } else {
                continue;
            };
            let Ok(event) = event else {
                continue;
            };
            let event: serde_json::Value = serde_json::from_str(event.raw().json().get())?;
            if event["type"] == "m.room.encrypted" && !migrated {
                continue;
            }
            let key = RoomKey::new(uuid::Uuid::nil(), room_id);
            let mut cache = self.cache.lock().await;
            cache.apply_matrix(&key, &event, own)?;
            cache.persist().await?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "keys_test.rs"]
mod tests;
