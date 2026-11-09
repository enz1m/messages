use super::*;
use base64::Engine;
use messages_core::{Attachment, MAX_ATTACHMENT_BYTES};
use messages_protocols::media::WireAttachment;
use std::io::{Cursor, Seek, SeekFrom, Write};
use whatsapp_rust::{
    UploadOptions,
    buffa::Message as _,
    wacore::download::{DownloadWriter, MediaType},
};

pub(super) fn attachments(raw: &wa::Message, id: &str) -> Vec<Attachment> {
    if raw.is_view_once()
        || raw.is_ephemeral()
        || raw.get_ephemeral_expiration().is_some_and(|v| v > 0)
    {
        return Vec::new();
    }
    let base = raw.get_base_message();
    // Retain only the encrypted media descriptor, never a complete message or thumbnail.
    let (name, mime, size, descriptor) = if let Some(file) = base.document_message.as_option() {
        let mut file = file.clone();
        file.jpeg_thumbnail = None;
        file.caption = None;
        file.context_info = MessageField::none();
        (
            file.file_name.clone().unwrap_or("File".into()),
            file.mimetype
                .clone()
                .unwrap_or("application/octet-stream".into()),
            file.file_length.unwrap_or(0),
            wa::Message {
                document_message: MessageField::some(file),
                ..Default::default()
            },
        )
    } else if let Some(image) = base.image_message.as_option() {
        let mut image = image.clone();
        image.jpeg_thumbnail = None;
        image.caption = None;
        image.context_info = MessageField::none();
        (
            "Photo.jpg".into(),
            image.mimetype.clone().unwrap_or("image/jpeg".into()),
            image.file_length.unwrap_or(0),
            wa::Message {
                image_message: MessageField::some(image),
                ..Default::default()
            },
        )
    } else if let Some(video) = base.video_message.as_option() {
        let mut video = video.clone();
        video.jpeg_thumbnail = None;
        video.caption = None;
        video.context_info = MessageField::none();
        (
            "Video.mp4".into(),
            video.mimetype.clone().unwrap_or("video/mp4".into()),
            video.file_length.unwrap_or(0),
            wa::Message {
                video_message: MessageField::some(video),
                ..Default::default()
            },
        )
    } else if let Some(audio) = base.audio_message.as_option() {
        let mut audio = audio.clone();
        audio.context_info = MessageField::none();
        (
            "Audio.ogg".into(),
            audio.mimetype.clone().unwrap_or("audio/ogg".into()),
            audio.file_length.unwrap_or(0),
            wa::Message {
                audio_message: MessageField::some(audio),
                ..Default::default()
            },
        )
    } else {
        return Vec::new();
    };
    let encoded = descriptor.encode_to_vec();
    if encoded.len() > 16 * 1024 {
        return Vec::new();
    }
    vec![Attachment {
        id: id.into(),
        name,
        mime,
        bytes: size,
        remote_url: Some(base64::engine::general_purpose::STANDARD.encode(encoded)),
    }]
}

struct BoundedSink(Cursor<Vec<u8>>);
impl Write for BoundedSink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.0.position().saturating_add(bytes.len() as u64) > MAX_ATTACHMENT_BYTES {
            return Err(std::io::Error::other("Attachment exceeds download limit"));
        }
        self.0.write(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl Seek for BoundedSink {
    fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
        self.0.seek(from)
    }
}
impl DownloadWriter for BoundedSink {
    fn truncate(&mut self, size: u64) -> std::io::Result<()> {
        self.0.get_mut().truncate(size as usize);
        Ok(())
    }
}

impl WhatsApp {
    pub(super) async fn attachment(&self, command: &Command) -> anyhow::Result<Value> {
        let cache = self.cache.lock().await;
        let key = RoomKey::new(uuid::Uuid::nil(), &command.chat_id);
        let file = cache
            .store
            .room(&key)
            .into_iter()
            .flat_map(|room| &room.messages)
            .filter(|m| !m.deleted)
            .flat_map(|m| &m.attachments)
            .find(|file| file.id == command.attachment_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("File is unavailable"))?;
        drop(cache);
        anyhow::ensure!(
            file.bytes <= MAX_ATTACHMENT_BYTES,
            "Attachment exceeds download limit"
        );
        let encoded = file
            .remote_url
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Missing descriptor"))?;
        anyhow::ensure!(encoded.len() <= 24 * 1024, "Invalid descriptor");
        let raw = wa::Message::decode_from_slice(
            &base64::engine::general_purpose::STANDARD.decode(encoded)?,
        )?;
        let sink = BoundedSink(Cursor::new(Vec::new()));
        let sink = if let Some(file) = raw.document_message.as_option() {
            self.client.download_to_writer(file, sink).await?
        } else if let Some(file) = raw.image_message.as_option() {
            self.client.download_to_writer(file, sink).await?
        } else if let Some(file) = raw.video_message.as_option() {
            self.client.download_to_writer(file, sink).await?
        } else if let Some(file) = raw.audio_message.as_option() {
            self.client.download_to_writer(file, sink).await?
        } else {
            anyhow::bail!("Invalid media descriptor")
        };
        let bytes = sink.0.into_inner();
        let size = bytes.len() as u64;
        let data = tokio::task::spawn_blocking(move || {
            base64::engine::general_purpose::STANDARD.encode(bytes)
        })
        .await?;
        Ok(
            json!({"attachment":WireAttachment { name: file.name, mime: file.mime, bytes: size, data }}),
        )
    }

    pub(super) async fn send_files(
        &self,
        jid: &Jid,
        command: &Command,
        context: Option<wa::ContextInfo>,
    ) -> anyhow::Result<Value> {
        let transaction = command
            .transaction_id
            .ok_or_else(|| anyhow::anyhow!("Missing transaction ID"))?;
        let mut last = None;
        for (index, file) in command.attachments.iter().enumerate() {
            let bytes = messages_protocols::media::decode(file)?;
            let image = matches!(file.mime.as_str(), "image/jpeg" | "image/png");
            let upload = self
                .client
                .upload(
                    bytes,
                    if image {
                        MediaType::Image
                    } else {
                        MediaType::Document
                    },
                    UploadOptions::default(),
                )
                .await?;
            let caption = (index == 0 && !command.body.is_empty()).then(|| command.body.clone());
            let context_info = context.clone().map(MessageField::some).unwrap_or_default();
            let content = if image {
                wa::Message {
                    image_message: MessageField::some(wa::message::ImageMessage {
                        url: Some(upload.url),
                        direct_path: Some(upload.direct_path),
                        mimetype: Some(file.mime.clone()),
                        media_key: Some(upload.media_key.to_vec()),
                        file_sha256: Some(upload.file_sha256.to_vec()),
                        file_enc_sha256: Some(upload.file_enc_sha256.to_vec()),
                        file_length: Some(upload.file_length),
                        media_key_timestamp: Some(upload.media_key_timestamp),
                        caption,
                        context_info,
                        ..Default::default()
                    }),
                    ..Default::default()
                }
            } else {
                wa::Message {
                    document_message: MessageField::some(wa::message::DocumentMessage {
                        url: Some(upload.url),
                        direct_path: Some(upload.direct_path),
                        mimetype: Some(file.mime.clone()),
                        file_name: Some(file.name.clone()),
                        media_key: Some(upload.media_key.to_vec()),
                        file_sha256: Some(upload.file_sha256.to_vec()),
                        file_enc_sha256: Some(upload.file_enc_sha256.to_vec()),
                        file_length: Some(upload.file_length),
                        media_key_timestamp: Some(upload.media_key_timestamp),
                        caption,
                        context_info,
                        ..Default::default()
                    }),
                    ..Default::default()
                }
            };
            let result = self.client.send_message(jid, content.clone()).await?;
            let sender = self
                .own_user()
                .ok_or_else(|| anyhow::anyhow!("Missing own identity"))?;
            let id = scoped_id(true, &sender, result.message_id.as_str());
            let files = attachments(&content, &id);
            let mut cache = self.cache.lock().await;
            events::merge(
                &mut cache,
                &command.chat_id,
                &command.chat_id,
                if command.chat_id.ends_with("@g.us") {
                    RoomKind::Group
                } else {
                    RoomKind::Direct
                },
                ChatMessage {
                    content_state: Some(messages_core::MessageContentState::Available),
                    id: id.clone(),
                    transaction: (index + 1 == command.attachments.len()).then_some(transaction),
                    sender,
                    sender_name: "You".into(),
                    own: true,
                    body: if index == 0 {
                        command.body.clone()
                    } else {
                        String::new()
                    },
                    timestamp: Utc::now(),
                    delivery: Delivery::Sent,
                    edited: false,
                    deleted: false,
                    reply_to: command.reply_to.clone(),
                    reactions: Vec::new(),
                    attachments: files,
                },
            )?;
            cache.persist().await?;
            last = Some(id);
        }
        Ok(json!({"message_id":last.ok_or_else(|| anyhow::anyhow!("No attachments"))?}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whatsapp_media_descriptors_roundtrip_without_capturing_body_or_thumbnail() {
        let raw = wa::Message {
            image_message: MessageField::some(wa::message::ImageMessage {
                mimetype: Some("image/jpeg".into()),
                file_length: Some(123),
                media_key: Some(vec![7; 32]),
                caption: Some("Private caption".into()),
                jpeg_thumbnail: Some(vec![9; 1000]),
                ..Default::default()
            }),
            ..Default::default()
        };
        let files = attachments(&raw, "sender|message");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].bytes, 123);
        let encoded = files[0].remote_url.as_ref().unwrap();
        let raw = wa::Message::decode_from_slice(
            &base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .unwrap(),
        )
        .unwrap();
        let image = raw.image_message.as_option().unwrap();
        assert!(image.jpeg_thumbnail.is_none());
        assert!(!image.context_info.is_set());
        assert_eq!(image.media_key.as_deref(), Some([7; 32].as_slice()));
    }

    #[test]
    fn whatsapp_media_download_limit_applies_before_writing() {
        let mut sink = BoundedSink(Cursor::new(Vec::new()));
        sink.seek(SeekFrom::Start(MAX_ATTACHMENT_BYTES)).unwrap();
        assert!(sink.write_all(b"x").is_err());
        assert!(sink.0.get_ref().is_empty());
    }
}
