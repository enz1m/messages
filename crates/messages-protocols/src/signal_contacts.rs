use serde_json::{Value, json};

use crate::{AdapterError, signal::Signal};
use messages_core::{Conversation, RoomKind};

impl Signal {
    pub(crate) fn resolve(&self, identity: &str) -> String {
        self.identities
            .lock()
            .ok()
            .and_then(|identities| identities.get(identity).cloned())
            .unwrap_or_else(|| identity.into())
    }

    pub(crate) fn address(
        &self,
        value: &Value,
        number: &str,
        uuid: &str,
        legacy: &str,
    ) -> Option<String> {
        let identities = self.identities.lock().ok()?;
        for field in [uuid, number, legacy] {
            if let Some(identity) = value[field]
                .as_str()
                .filter(|identity| !identity.is_empty())
                && let Some(canonical) = identities.get(identity)
            {
                return Some(canonical.clone());
            }
        }
        [number, uuid, legacy].iter().find_map(|field| {
            value[*field]
                .as_str()
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        })
    }

    pub(crate) fn remember_contact(&self, contact: &Value) -> Option<String> {
        let number = contact["number"].as_str().filter(|value| !value.is_empty());
        let uuid = contact["uuid"].as_str().filter(|value| !value.is_empty());
        let mut identities = self.identities.lock().ok()?;
        let canonical = uuid
            .filter(|uuid| self.config.rooms.iter().any(|room| room == uuid))
            .map(str::to_owned)
            .or_else(|| uuid.and_then(|uuid| identities.get(uuid).cloned()))
            .or_else(|| number.or(uuid).map(str::to_owned))?;
        for identity in [number, uuid].into_iter().flatten() {
            identities.insert(identity.into(), canonical.clone());
        }
        Some(canonical)
    }

    pub(crate) async fn find_recipient(
        &self,
        recipient: &str,
    ) -> Result<Conversation, AdapterError> {
        let recipient = recipient
            .trim()
            .trim_start_matches('@')
            .trim_start_matches("u:");
        let phone = recipient.starts_with('+')
            && (8..=16).contains(&recipient.len())
            && recipient[1..].bytes().all(|byte| byte.is_ascii_digit());
        let username = recipient
            .rsplit_once('.')
            .is_some_and(|(name, discriminator)| {
                (3..=32).contains(&name.len())
                    && name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                    && (2..=9).contains(&discriminator.len())
                    && discriminator.bytes().all(|byte| byte.is_ascii_digit())
            });
        if !phone && !username {
            return Err(AdapterError::Configuration("Enter a phone number with its country code or a Signal username, such as name.123.".into()));
        }
        let params = if phone {
            json!({"recipient":[recipient]})
        } else {
            json!({"username":[recipient]})
        };
        let result = self.rpc("getUserStatus", params).await?;
        let entry = result
            .as_array()
            .into_iter()
            .flatten()
            .find(|entry| entry["isRegistered"] == true && entry["recipient"] == recipient)
            .ok_or_else(|| {
                AdapterError::Configuration("That account could not be found on Signal.".into())
            })?;
        let id = self
            .remember_contact(entry)
            .unwrap_or_else(|| recipient.into());
        Ok(self.conversation(&id, recipient, RoomKind::Direct))
    }
}
