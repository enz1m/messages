use std::time::Duration;

use base64::{Engine, engine::general_purpose::STANDARD};
use messages_core::{Attachment, MAX_ATTACHMENT_BYTES, MAX_ATTACHMENTS, RoomKey, SendRequest};
use reqwest::Method;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;

use crate::{Adapter, AdapterError, ManagedConnection, http::Http, signal::Signal};

pub(crate) static MEDIA_JOBS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

pub(crate) fn attachments(data: &Value) -> Vec<Attachment> {
    data["attachments"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|file| {
            let id = file["id"].as_str()?.to_owned();
            let mime = file["contentType"]
                .as_str()
                .unwrap_or("application/octet-stream");
            let fallback = match mime {
                "image/jpeg" => "Photo.jpg",
                "image/png" => "Photo.png",
                "image/webp" => "Photo.webp",
                "image/gif" => "Animation.gif",
                "video/mp4" => "Video.mp4",
                "video/quicktime" => "Video.mov",
                "audio/ogg" if file["isVoiceNote"] == true => "Voice message.ogg",
                "audio/ogg" => "Audio.ogg",
                "audio/mpeg" => "Audio.mp3",
                "audio/mp4" => "Audio.m4a",
                "application/pdf" => "Document.pdf",
                _ => "Attachment",
            };
            Some(Attachment {
                id,
                name: file["filename"]
                    .as_str()
                    .filter(|name| !name.is_empty())
                    .unwrap_or(fallback)
                    .into(),
                mime: mime.into(),
                bytes: file["size"].as_u64().unwrap_or(0),
                remote_url: None,
            })
        })
        .collect()
}

impl Signal {
    pub(crate) async fn rpc_response(
        &self,
        method: &str,
        mut params: Value,
        id: &str,
        limit: usize,
    ) -> Result<Value, AdapterError> {
        params["account"] = json!(self.active_user()?);
        let response = self
            .http
            .request(Method::POST, self.http.url(&["api", "v1", "rpc"])?)
            .timeout(Duration::from_secs(
                if matches!(method, "send" | "getAttachment") {
                    120
                } else {
                    35
                },
            ))
            .json(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .send()
            .await
            .map_err(|_| AdapterError::Network)?;
        Http::decode_limited(response, limit).await
    }

    pub(crate) async fn add_files(
        &self,
        request: &SendRequest,
        params: &mut Value,
    ) -> Result<(), AdapterError> {
        let total = request
            .attachments
            .iter()
            .try_fold(0_u64, |sum, file| sum.checked_add(file.bytes));
        if request.attachments.len() > MAX_ATTACHMENTS
            || total.is_none_or(|bytes| bytes > MAX_ATTACHMENT_BYTES)
        {
            return Err(AdapterError::Configuration(
                "Attach at most 10 files, up to 100 MB in total.".into(),
            ));
        }
        let mut files = Vec::with_capacity(request.attachments.len());
        for file in &request.attachments {
            let source = tokio::fs::File::open(&file.path).await.map_err(|_| {
                AdapterError::Configuration(
                    "An attached file is no longer available. Attach it again.".into(),
                )
            })?;
            let metadata = source.metadata().await.map_err(|_| AdapterError::Network)?;
            if !metadata.is_file() || metadata.len() != file.bytes {
                return Err(AdapterError::Configuration(
                    "An attached file has changed. Attach it again.".into(),
                ));
            }
            if matches!(self.config.managed, Some(ManagedConnection::Signal)) {
                files.push(
                    file.path
                        .to_str()
                        .ok_or_else(|| {
                            AdapterError::Configuration("The file path is not valid UTF-8.".into())
                        })?
                        .to_owned(),
                );
            } else {
                let mut bytes = Vec::new();
                source
                    .take(file.bytes.saturating_add(1))
                    .read_to_end(&mut bytes)
                    .await
                    .map_err(|_| AdapterError::Network)?;
                if bytes.len() as u64 != file.bytes {
                    return Err(AdapterError::Configuration(
                        "An attached file has changed. Attach it again.".into(),
                    ));
                }
                let mime = file.mime.clone();
                let name: String =
                    url::form_urlencoded::byte_serialize(file.name.as_bytes()).collect();
                let uri = tokio::task::spawn_blocking(move || {
                    format!(
                        "data:{mime};filename={name};base64,{}",
                        STANDARD.encode(bytes)
                    )
                })
                .await
                .map_err(|_| AdapterError::Network)?;
                files.push(uri);
            }
        }
        if !files.is_empty() {
            params["attachments"] = json!(files);
        }
        Ok(())
    }

    pub(crate) async fn fetch_file(
        &self,
        room: &RoomKey,
        attachment: &Attachment,
    ) -> Result<Vec<u8>, AdapterError> {
        self.validate_room(room)?;
        if attachment.bytes > MAX_ATTACHMENT_BYTES {
            return Err(AdapterError::Unsupported(
                "This file exceeds the 100 MB download limit.".into(),
            ));
        }
        let _permit = MEDIA_JOBS
            .acquire()
            .await
            .map_err(|_| AdapterError::Network)?;
        let mut params = if let Some(group) = room.room.strip_prefix("group:") {
            json!({"groupId":group})
        } else {
            json!({"recipient":room.room})
        };
        params["id"] = json!(attachment.id);
        let id = uuid::Uuid::new_v4().to_string();
        let limit = (MAX_ATTACHMENT_BYTES as usize).div_ceil(3) * 4 + 4096;
        let mut response = self
            .rpc_response("getAttachment", params, &id, limit)
            .await?;
        if response["id"] != id || !response["error"].is_null() {
            return Err(AdapterError::InvalidData(
                "The attached file could not be retrieved.".into(),
            ));
        }
        let data = match response["result"]["data"].take() {
            Value::String(data) => data,
            _ => return Err(AdapterError::InvalidData("missing attachment data".into())),
        };
        drop(response);
        let bytes = tokio::task::spawn_blocking(move || STANDARD.decode(data))
            .await
            .map_err(|_| AdapterError::Network)?
            .map_err(|_| AdapterError::InvalidData("invalid attachment encoding".into()))?;
        if bytes.len() as u64 > MAX_ATTACHMENT_BYTES
            || (attachment.bytes > 0 && bytes.len() as u64 != attachment.bytes)
        {
            return Err(AdapterError::InvalidData(
                "attachment size does not match".into(),
            ));
        }
        Ok(bytes)
    }
}
