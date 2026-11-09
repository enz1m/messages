use std::{path::PathBuf, sync::Arc};

use messages_core::{
    Capabilities, ChatMessage, Conversation, Encryption, MessageStore, RoomKey, RoomKind,
};
use tokio::{io::AsyncWriteExt, sync::Mutex};

pub type SharedCache = Arc<Mutex<Cache>>;

#[derive(serde::Serialize, serde::Deserialize, Default)]
pub struct Cache {
    #[serde(flatten)]
    pub store: MessageStore,
    #[serde(default)]
    pub matrix: messages_protocols::matrix_timeline::MatrixTimeline,
    #[cfg(feature = "whatsapp")]
    #[serde(default)]
    pub(crate) whatsapp: crate::whatsapp::events::Timeline,
    #[serde(skip)]
    path: PathBuf,
    #[serde(skip)]
    pub(crate) typing: std::collections::BTreeMap<
        (String, String),
        (messages_protocols::TypingUpdate, std::time::Instant),
    >,
}

impl Cache {
    pub async fn open(path: PathBuf) -> anyhow::Result<SharedCache> {
        let mut cache: Self = match tokio::fs::read(&path).await {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => return Err(error.into()),
        };
        cache.path = path;
        for room in &mut cache.store.conversations {
            for message in &mut room.messages {
                if cache.matrix.awaiting_keys(&room.key.room, &message.id) && !message.deleted {
                    message.content_state =
                        Some(messages_core::MessageContentState::WaitingForKeys);
                    message.body.clear();
                }
            }
        }
        Ok(Arc::new(Mutex::new(cache)))
    }

    #[cfg(any(feature = "matrix", feature = "whatsapp"))]
    pub(crate) fn set_typing(&mut self, chat: String, actor: String, active: bool) {
        if self.typing.len() >= 256 {
            self.typing
                .retain(|_, (_, time)| time.elapsed().as_secs() < 8);
        }
        if self.typing.len() < 256 {
            self.typing.insert(
                (chat.clone(), actor.clone()),
                (
                    messages_protocols::TypingUpdate {
                        room: RoomKey::new(uuid::Uuid::nil(), chat),
                        name: actor.clone(),
                        actor,
                        active,
                    },
                    std::time::Instant::now(),
                ),
            );
        }
    }

    pub fn apply_matrix(
        &mut self,
        key: &RoomKey,
        event: &serde_json::Value,
        own: &str,
    ) -> anyhow::Result<()> {
        let relation_placeholder = (event["type"] == "m.reaction"
            || event["content"]["m.relates_to"]["rel_type"] == "m.replace")
            && event["event_id"]
                .as_str()
                .is_some_and(|id| self.matrix.awaiting_keys(&key.room, id));
        let message = self.matrix.apply(&key.room, event, own)?;
        if relation_placeholder
            && let Some(room) = self.store.room_mut(key)
            && let Some(message) = room
                .messages
                .iter_mut()
                .find(|message| Some(message.id.as_str()) == event["event_id"].as_str())
        {
            message.content_state = Some(messages_core::MessageContentState::Relation);
            message.deleted = true;
            message.body.clear();
            message.attachments.clear();
            message.reply_to = None;
            message.reactions.clear();
        }
        if let Some(message) = message {
            self.store.merge_messages(key, vec![message])?;
        }
        Ok(())
    }

    pub fn upsert(
        &mut self,
        chat: &str,
        title: &str,
        kind: RoomKind,
        encryption: Encryption,
        capabilities: Capabilities,
        message: ChatMessage,
    ) -> anyhow::Result<()> {
        let key = RoomKey::new(uuid::Uuid::nil(), chat);
        if self.store.room(&key).is_none() {
            self.store.conversations.push(Conversation {
                key: key.clone(),
                title: title.into(),
                subtitle: String::new(),
                kind,
                members: Vec::new(),
                unread: 0,
                pinned: false,
                muted: false,
                muted_until: None,
                avatar_revision: 0,
                group_name: None,
                parent_room: None,
                archived: false,
                encryption,
                capabilities,
                messages: Vec::new(),
                draft: String::new(),
                draft_attachments: Vec::new(),
                reply_to: None,
                has_older: false,
                pagination: None,
            });
        }
        self.store.merge_messages(&key, vec![message])?;
        Ok(())
    }

    pub async fn persist(&self) -> anyhow::Result<()> {
        let temporary = self.path.with_extension("tmp");
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temporary)
            .await?;
        file.write_all(&serde_json::to_vec(self)?).await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(temporary, &self.path).await?;
        Ok(())
    }
}
