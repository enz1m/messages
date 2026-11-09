use super::Matrix;
use crate::Command;
use matrix_sdk::{
    attachment::AttachmentConfig,
    ruma::{
        OwnedTransactionId,
        events::room::{MediaSource, message::TextMessageEventContent},
    },
};
use messages_core::{MAX_ATTACHMENT_BYTES, RoomKey};
use messages_protocols::media::WireAttachment;
use serde_json::{Value, json};

impl Matrix {
    pub(super) async fn send_files(
        &self,
        room: &matrix_sdk::Room,
        command: Command,
    ) -> anyhow::Result<Value> {
        let transaction = command
            .transaction_id
            .ok_or_else(|| anyhow::anyhow!("Missing transaction ID"))?;
        let mut last = None;
        for (index, file) in command.attachments.into_iter().enumerate() {
            let bytes = messages_protocols::media::decode(&file)?;
            let mime: mime::Mime = file.mime.parse()?;
            let mut config = AttachmentConfig::new()
                .txn_id(OwnedTransactionId::from(format!("{transaction}-{index}")));
            if index == 0 && !command.body.trim().is_empty() {
                config = config.caption(Some(TextMessageEventContent::plain(command.body.clone())));
            }
            if let Some(reply) = &command.reply_to {
                config.reply = Some(matrix_sdk::room::reply::Reply {
                    event_id: matrix_sdk::ruma::EventId::parse(reply)?,
                    enforce_thread: matrix_sdk::room::reply::EnforceThread::Unthreaded,
                    add_mentions: matrix_sdk::ruma::events::room::message::AddMentions::No,
                });
            }
            let result = room
                .send_attachment(file.name, &mime, bytes, config)
                .await?;
            last = Some(result.event_id);
        }
        Ok(json!({"message_id":last.ok_or_else(|| anyhow::anyhow!("No attachments"))?}))
    }

    pub(super) async fn download_attachment(&self, command: &Command) -> anyhow::Result<Value> {
        let key = RoomKey::new(uuid::Uuid::nil(), &command.chat_id);
        let file = self
            .cache
            .lock()
            .await
            .store
            .room(&key)
            .into_iter()
            .flat_map(|room| &room.messages)
            .filter(|message| !message.deleted)
            .flat_map(|m| &m.attachments)
            .find(|file| file.id == command.attachment_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("File is unavailable"))?;
        anyhow::ensure!(
            file.bytes <= MAX_ATTACHMENT_BYTES,
            "File exceeds download limit"
        );
        let url = file
            .remote_url
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Missing media source"))?;
        let source = if url.starts_with('{') {
            MediaSource::Encrypted(Box::new(serde_json::from_str(url)?))
        } else {
            MediaSource::Plain(url.into())
        };
        let source_url = match &source {
            MediaSource::Encrypted(file) => &file.url,
            MediaSource::Plain(url) => url,
        };
        let (server, media) = source_url.parts()?;
        let http = matrix_sdk::reqwest::Client::builder()
            .redirect(matrix_sdk::reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(60))
            .build()?;
        let mut url = self.client.homeserver();
        url.path_segments_mut()
            .map_err(|_| anyhow::anyhow!("Invalid homeserver"))?
            .clear()
            .extend([
                "_matrix",
                "client",
                "v1",
                "media",
                "download",
                server.as_str(),
                media,
            ]);
        url.set_query(None);
        url.set_fragment(None);
        let token = self
            .client
            .access_token()
            .ok_or_else(|| anyhow::anyhow!("Sign in again"))?;
        let mut response = http.get(url.clone()).bearer_auth(&token).send().await?;
        if response.status() == matrix_sdk::reqwest::StatusCode::NOT_FOUND {
            url.path_segments_mut()
                .map_err(|_| anyhow::anyhow!("Invalid homeserver"))?
                .clear()
                .extend(["_matrix", "media", "v3", "download", server.as_str(), media]);
            url.set_query(Some("allow_redirect=false"));
            response = http.get(url).send().await?;
        }
        anyhow::ensure!(response.status().is_success(), "Media download failed");
        anyhow::ensure!(
            response
                .content_length()
                .is_none_or(|size| size <= MAX_ATTACHMENT_BYTES),
            "File exceeds download limit"
        );
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            anyhow::ensure!(
                bytes.len().saturating_add(chunk.len()) as u64 <= MAX_ATTACHMENT_BYTES,
                "File exceeds download limit"
            );
            bytes.extend_from_slice(&chunk);
        }
        let bytes = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<u8>> {
            if let MediaSource::Encrypted(file) = source {
                let mut cursor = std::io::Cursor::new(bytes);
                let mut reader =
                    matrix_sdk_crypto::AttachmentDecryptor::new(&mut cursor, (*file).into())?;
                let mut decrypted = Vec::new();
                std::io::Read::read_to_end(&mut reader, &mut decrypted)?;
                anyhow::ensure!(
                    decrypted.len() as u64 <= MAX_ATTACHMENT_BYTES,
                    "File exceeds download limit"
                );
                Ok(decrypted)
            } else {
                Ok(bytes)
            }
        })
        .await??;
        use base64::Engine;
        let size = bytes.len() as u64;
        let data = tokio::task::spawn_blocking(move || {
            base64::engine::general_purpose::STANDARD.encode(bytes)
        })
        .await?;
        Ok(
            json!({"attachment":WireAttachment { name: file.name, mime: file.mime, bytes: size, data }}),
        )
    }
}
