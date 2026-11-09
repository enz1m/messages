use crate::AdapterError;
use base64::{Engine, engine::general_purpose::STANDARD};
use messages_core::{MAX_ATTACHMENT_BYTES, MAX_ATTACHMENTS, OutgoingAttachment};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;

#[derive(Clone, Serialize, Deserialize)]
pub struct WireAttachment {
    pub name: String,
    pub mime: String,
    pub bytes: u64,
    pub data: String,
}

pub async fn read(file: &OutgoingAttachment) -> Result<Vec<u8>, AdapterError> {
    if file.bytes > MAX_ATTACHMENT_BYTES
        || file.name.len() > 255
        || file.mime.len() > 128
        || file.name.chars().any(char::is_control)
    {
        return Err(AdapterError::Configuration(
            "This attachment exceeds the file limits.".into(),
        ));
    }
    let source = tokio::fs::File::open(&file.path).await.map_err(|_| {
        AdapterError::Configuration("An attached file is no longer available.".into())
    })?;
    let metadata = source.metadata().await.map_err(|_| AdapterError::Network)?;
    if !metadata.is_file() || metadata.len() != file.bytes {
        return Err(AdapterError::Configuration(
            "An attached file has changed. Attach it again.".into(),
        ));
    }
    let mut bytes = Vec::new();
    source
        .take(file.bytes + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| AdapterError::Network)?;
    if bytes.len() as u64 != file.bytes {
        return Err(AdapterError::Configuration(
            "An attached file has changed. Attach it again.".into(),
        ));
    }
    Ok(bytes)
}

pub async fn encode(files: &[OutgoingAttachment]) -> Result<Vec<WireAttachment>, AdapterError> {
    validate_total(files.len(), files.iter().map(|file| file.bytes))?;
    let mut encoded = Vec::new();
    for file in files {
        let bytes = read(file).await?;
        let data = tokio::task::spawn_blocking(move || STANDARD.encode(bytes))
            .await
            .map_err(|_| AdapterError::Network)?;
        encoded.push(WireAttachment {
            name: file.name.clone(),
            mime: file.mime.clone(),
            bytes: file.bytes,
            data,
        });
    }
    Ok(encoded)
}

pub fn validate_total(
    count: usize,
    sizes: impl IntoIterator<Item = u64>,
) -> Result<(), AdapterError> {
    let total = sizes
        .into_iter()
        .try_fold(0u64, |sum, size| sum.checked_add(size));
    if count > MAX_ATTACHMENTS || total.is_none_or(|sum| sum > MAX_ATTACHMENT_BYTES) {
        return Err(AdapterError::Configuration(
            "Attach at most 10 files, up to 100 MB in total.".into(),
        ));
    }
    Ok(())
}

pub fn decode(file: &WireAttachment) -> Result<Vec<u8>, AdapterError> {
    if file.bytes > MAX_ATTACHMENT_BYTES
        || file.data.len() > (MAX_ATTACHMENT_BYTES as usize).div_ceil(3) * 4
    {
        return Err(AdapterError::InvalidData(
            "Attachment exceeds size limit".into(),
        ));
    }
    let bytes = STANDARD
        .decode(&file.data)
        .map_err(|_| AdapterError::InvalidData("Invalid attachment encoding".into()))?;
    if bytes.len() as u64 != file.bytes {
        return Err(AdapterError::InvalidData("Attachment size mismatch".into()));
    }
    Ok(bytes)
}

pub(crate) async fn response(mut response: reqwest::Response) -> Result<Vec<u8>, AdapterError> {
    crate::http::Http::check_status(&response)?;
    if response
        .content_length()
        .is_some_and(|bytes| bytes > MAX_ATTACHMENT_BYTES)
    {
        return Err(AdapterError::InvalidData(
            "Attachment exceeds size limit".into(),
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| AdapterError::Network)? {
        if bytes.len().saturating_add(chunk.len()) as u64 > MAX_ATTACHMENT_BYTES {
            return Err(AdapterError::InvalidData(
                "Attachment exceeds size limit".into(),
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
