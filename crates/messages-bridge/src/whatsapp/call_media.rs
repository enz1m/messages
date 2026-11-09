use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    task::JoinHandle,
};
use whatsapp_rust::voip::VideoFrame;

pub(super) struct Media {
    tasks: Vec<JoinHandle<()>>,
    camera_task: Option<JoinHandle<()>>,
    pub failed: Arc<AtomicBool>,
    pub preview: Arc<Mutex<Option<Vec<u8>>>>,
    pub microphone: async_channel::Receiver<Vec<i16>>,
    pub speaker: async_channel::Sender<Vec<i16>>,
    pub video: Option<(
        async_channel::Receiver<Vec<u8>>,
        async_channel::Sender<VideoFrame>,
    )>,
}
impl Drop for Media {
    fn drop(&mut self) {
        if let Some(task) = &self.camera_task {
            task.abort();
        }
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl Media {
    pub async fn open(devices: &messages_media::Selection, video: bool) -> anyhow::Result<Self> {
        messages_media::validate_selection(devices, video).map_err(anyhow::Error::msg)?;
        let failed = Arc::new(AtomicBool::new(false));
        let (mic_tx, microphone) = async_channel::bounded(8);
        let (speaker, speaker_rx) = async_channel::bounded::<Vec<i16>>(8);
        let mut input = messages_media::audio_input(devices).map_err(anyhow::Error::msg)?;
        let mut reader = input
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("Microphone unavailable"))?;
        let mut first = [0u8; 1920];
        tokio::time::timeout(
            std::time::Duration::from_secs(4),
            reader.read_exact(&mut first),
        )
        .await??;
        let mut output = messages_media::audio_output(devices).map_err(anyhow::Error::msg)?;
        let mut writer = output
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("Speaker unavailable"))?;
        writer.write_all(&[0u8; 1920]).await?;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        anyhow::ensure!(output.try_wait()?.is_none(), "Speaker could not be opened");
        let mut media = Self {
            tasks: Vec::new(),
            camera_task: None,
            failed,
            preview: Arc::new(Mutex::new(None)),
            microphone,
            speaker,
            video: None,
        };
        let status = media.failed.clone();
        media.tasks.push(tokio::spawn(async move {
            let _child = input;
            let mut bytes = first;
            loop {
                let samples = bytes
                    .chunks_exact(2)
                    .map(|b| i16::from_le_bytes([b[0], b[1]]))
                    .collect();
                if mic_tx.send(samples).await.is_err() {
                    return;
                }
                if reader.read_exact(&mut bytes).await.is_err() {
                    status.store(true, Ordering::Release);
                    return;
                }
            }
        }));
        let status = media.failed.clone();
        media.tasks.push(tokio::spawn(async move {
            let _child = output;
            while let Ok(samples) = speaker_rx.recv().await {
                if samples.len() > 32000 {
                    status.store(true, Ordering::Release);
                    return;
                }
                let bytes: Vec<u8> = samples.into_iter().flat_map(i16::to_le_bytes).collect();
                if writer.write_all(&bytes).await.is_err() {
                    status.store(true, Ordering::Release);
                    return;
                }
            }
        }));
        if video {
            let source = media.camera(devices)?;
            let (sink, received) = async_channel::bounded::<VideoFrame>(4);
            let mut decoder = messages_media::decode_video().map_err(anyhow::Error::msg)?;
            let mut input = decoder
                .stdin
                .take()
                .ok_or_else(|| anyhow::anyhow!("Video decoder unavailable"))?;
            let mut output = decoder
                .stdout
                .take()
                .ok_or_else(|| anyhow::anyhow!("Video decoder unavailable"))?;
            let preview = media.preview.clone();
            let status = media.failed.clone();
            media.tasks.push(tokio::spawn(async move {
                let _child = decoder;
                let feed = async {
                    let mut ready = false;
                    while let Ok(frame) = received.recv().await {
                        ready |= frame.keyframe;
                        if !ready {
                            continue;
                        }
                        if frame.data.len() > 2 * 1024 * 1024
                            || input.write_all(&frame.data).await.is_err()
                        {
                            break;
                        }
                    }
                };
                let frames = async {
                    while let Ok(frame) = messages_media::read_png(&mut output).await {
                        if let Ok(mut slot) = preview.lock() {
                            *slot = Some(frame);
                        }
                    }
                };
                tokio::select! { _ = feed => {}, _ = frames => {} }
                status.store(true, Ordering::Release);
            }));
            media.video = Some((source, sink));
        }
        Ok(media)
    }

    pub fn stop_camera(&mut self) {
        if let Some(task) = self.camera_task.take() {
            task.abort();
        }
    }

    pub fn camera(
        &mut self,
        devices: &messages_media::Selection,
    ) -> anyhow::Result<async_channel::Receiver<Vec<u8>>> {
        let mut child = messages_media::camera_stream(devices).map_err(anyhow::Error::msg)?;
        let mut reader = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("Camera unavailable"))?;
        let (send, receive) = async_channel::bounded(2);
        let failed = self.failed.clone();
        if let Some(task) = self.camera_task.take() {
            task.abort();
        }
        self.camera_task = Some(tokio::spawn(async move {
            let _child = child;
            let mut buffer = Vec::new();
            let mut unit = Vec::new();
            let mut chunk = [0; 16384];
            loop {
                let Ok(n) = reader.read(&mut chunk).await else {
                    failed.store(true, Ordering::Release);
                    return;
                };
                if n == 0 || buffer.len() + n > 2 * 1024 * 1024 {
                    failed.store(true, Ordering::Release);
                    return;
                }
                buffer.extend_from_slice(&chunk[..n]);
                while let Some((end, prefix)) = next_nal(&buffer) {
                    let nal: Vec<u8> = buffer.drain(..end).collect();
                    let start = if nal.starts_with(&[0, 0, 0, 1]) { 4 } else { 3 };
                    if nal.get(start).is_some_and(|b| b & 31 == 9)
                        && !unit.is_empty()
                        && send.send(std::mem::take(&mut unit)).await.is_err()
                    {
                        return;
                    }
                    if unit.len() + nal.len() > 2 * 1024 * 1024 {
                        failed.store(true, Ordering::Release);
                        return;
                    }
                    unit.extend_from_slice(&nal);
                    let _ = prefix;
                }
            }
        }));
        Ok(receive)
    }
}

fn next_nal(bytes: &[u8]) -> Option<(usize, usize)> {
    let first = if bytes.starts_with(&[0, 0, 0, 1]) {
        4
    } else if bytes.starts_with(&[0, 0, 1]) {
        3
    } else {
        return None;
    };
    for i in first..bytes.len().saturating_sub(2) {
        if bytes[i..].starts_with(&[0, 0, 0, 1]) {
            return Some((i, 4));
        }
        if bytes[i..].starts_with(&[0, 0, 1]) {
            return Some((i, 3));
        }
    }
    None
}

#[cfg(all(test, feature = "call-fixtures"))]
mod tests {
    use super::*;
    #[tokio::test]
    async fn synthetic_audio_and_h264_decode_through_the_owned_media_pipes() {
        assert!(messages_media::synthetic());
        let media = Media::open(
            &messages_media::Selection {
                microphone: "test-input".into(),
                speaker: "test-output".into(),
                camera: "test-camera".into(),
            },
            true,
        )
        .await
        .unwrap();
        let pcm = tokio::time::timeout(std::time::Duration::from_secs(3), media.microphone.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pcm.len(), 960);
        assert!(pcm.iter().any(|s| *s != 0));
        media.speaker.send(pcm).await.unwrap();
        let (source, sink) = media.video.as_ref().unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let frame = VideoFrame::new(source.recv().await.unwrap());
                sink.send(frame).await.unwrap();
                if media.preview.lock().unwrap().is_some() {
                    break;
                }
            }
        })
        .await
        .unwrap();
        assert!(
            media
                .preview
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .starts_with(b"\x89PNG\r\n\x1a\n")
        );
        assert!(!media.failed.load(Ordering::Acquire));
        let tasks: Vec<_> = media
            .tasks
            .iter()
            .map(|t| t.abort_handle())
            .chain(media.camera_task.iter().map(|t| t.abort_handle()))
            .collect();
        drop(media);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while tasks.iter().any(|t| !t.is_finished()) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
}
