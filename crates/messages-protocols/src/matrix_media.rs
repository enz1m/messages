use crate::{
    Adapter, AdapterError,
    http::Http,
    matrix::{Matrix, string},
};
use messages_core::{Attachment, RoomKey, SendRequest};
use reqwest::Method;
use serde_json::json;

impl Matrix {
    pub(crate) async fn send_files(&self, request: &SendRequest) -> Result<String, AdapterError> {
        self.require_plaintext(&request.room).await?;
        crate::media::validate_total(
            request.attachments.len(),
            request.attachments.iter().map(|f| f.bytes),
        )?;
        let mut last = None;
        for (index, file) in request.attachments.iter().enumerate() {
            let bytes = crate::media::read(file).await?;
            let response = self
                .http
                .request(
                    Method::POST,
                    self.http.url(&["_matrix", "media", "v3", "upload"])?,
                )
                .query(&[("filename", &file.name)])
                .header(reqwest::header::CONTENT_TYPE, &file.mime)
                .body(bytes)
                .send()
                .await
                .map_err(|_| AdapterError::Network)?;
            let uploaded = Http::decode(response).await?;
            let url = string(&uploaded, "content_uri")?;
            let kind = if file.mime.starts_with("image/") {
                "m.image"
            } else if file.mime.starts_with("video/") {
                "m.video"
            } else if file.mime.starts_with("audio/") {
                "m.audio"
            } else {
                "m.file"
            };
            let mut content = json!({"msgtype":kind,"body":if index == 0 && !request.body.is_empty() { &request.body } else { &file.name }, "filename":file.name,"url":url,"info":{"mimetype":file.mime,"size":file.bytes}});
            if let Some(reply) = &request.reply_to {
                content["m.relates_to"] = json!({"m.in_reply_to":{"event_id":reply}});
            }
            last = Some(
                self.event(
                    &request.room,
                    "m.room.message",
                    &format!("{}-{index}", request.transaction),
                    content,
                )
                .await?,
            );
        }
        last.ok_or_else(|| AdapterError::InvalidData("No attachments".into()))
    }

    pub(crate) async fn download_file(
        &self,
        room: &RoomKey,
        file: &Attachment,
    ) -> Result<Vec<u8>, AdapterError> {
        self.validate_room(room)?;
        if file.bytes > messages_core::MAX_ATTACHMENT_BYTES {
            return Err(AdapterError::InvalidData(
                "File exceeds download limit".into(),
            ));
        }
        let source = url::Url::parse(file.remote_url.as_deref().unwrap_or("")).map_err(|_| {
            AdapterError::Unsupported(
                "Use the native Matrix connection for encrypted files.".into(),
            )
        })?;
        if source.scheme() != "mxc"
            || !source.username().is_empty()
            || source.password().is_some()
            || source.query().is_some()
            || source.fragment().is_some()
        {
            return Err(AdapterError::InvalidData(
                "Invalid Matrix media source".into(),
            ));
        }
        let server = source
            .host_str()
            .ok_or_else(|| AdapterError::InvalidData("Missing media server".into()))?;
        let server = if let Some(port) = source.port() {
            format!("{server}:{port}")
        } else {
            server.to_owned()
        };
        let id = source.path().trim_start_matches('/');
        if id.is_empty() || id.contains('/') {
            return Err(AdapterError::InvalidData(
                "Invalid Matrix media identity".into(),
            ));
        }
        let response = self
            .http
            .request(
                Method::GET,
                self.http
                    .url(&["_matrix", "client", "v1", "media", "download", &server, id])?,
            )
            .send()
            .await
            .map_err(|_| AdapterError::Network)?;
        crate::media::response(response).await
    }
}
