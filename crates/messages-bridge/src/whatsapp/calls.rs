use super::call_media::Media;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use messages_core::{Conversation, Encryption, RoomKey, RoomKind};
use messages_protocols::calls::{
    CallCommand, CallInvite, CallKind, CallPhase, CallRequest, CallSnapshot,
};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};
use tokio::sync::{Mutex, Notify};
use uuid::Uuid;
use whatsapp_rust::{
    prelude::*,
    voip::{CallEvent, CallHandle},
    wacore::types::call::{CallAction, IncomingCall},
};

#[derive(Default)]
pub(super) struct Manager {
    state: Mutex<State>,
    setup_pending: Arc<AtomicBool>,
}
#[derive(Default)]
struct State {
    slot: Option<Slot>,
    retired: VecDeque<(Uuid, Instant)>,
    blocked_until: Option<Instant>,
}
struct Slot {
    chat: String,
    snapshot: CallSnapshot,
    handle: Option<CallHandle>,
    incoming: Option<IncomingCall>,
    media: Option<Media>,
    started: Instant,
    cancel: Arc<Notify>,
    cancelled: Arc<AtomicBool>,
    accepted: bool,
    devices: Option<messages_media::Selection>,
}

impl Manager {
    pub async fn invites(&self) -> Vec<CallInvite> {
        let state = self.state.lock().await;
        state
            .slot
            .as_ref()
            .filter(|s| {
                s.snapshot.phase == CallPhase::Incoming && s.started.elapsed().as_secs() < 45
            })
            .map(|s| {
                vec![CallInvite {
                    room: RoomKey::new(Uuid::nil(), &s.chat),
                    call: s.snapshot.clone(),
                }]
            })
            .unwrap_or_default()
    }

    pub async fn receive(self: &Arc<Self>, event: &Event, cache: &crate::cache::SharedCache) {
        if matches!(event, Event::Disconnected(_) | Event::LoggedOut(_)) {
            let mut state = self.state.lock().await;
            if let Some(slot) = state.slot.as_mut() {
                slot.cancelled.store(true, Ordering::Release);
                slot.cancel.notify_waiters();
                slot.media.take();
                slot.snapshot.phase = CallPhase::Ended;
                let handle = slot.handle.take();
                drop(state);
                if let Some(handle) = handle {
                    handle.hangup_local().await;
                }
            }
            return;
        }
        let Event::IncomingCall(incoming) = event else {
            return;
        };
        let mut state = self.state.lock().await;
        if let Some(slot) = state.slot.as_mut() {
            let matches = slot
                .handle
                .as_ref()
                .is_some_and(|h| h.call_id() == incoming.action.call_id())
                || slot
                    .incoming
                    .as_ref()
                    .is_some_and(|i| i.action.call_id() == incoming.action.call_id());
            if matches {
                if slot.snapshot.phase.finished() {
                    return;
                }
                match incoming.action {
                    CallAction::Accept { .. } => {
                        slot.accepted = true;
                        slot.snapshot.phase = CallPhase::Connected;
                    }
                    CallAction::Terminate { .. } | CallAction::Reject { reason: None, .. } => {
                        slot.cancelled.store(true, Ordering::Release);
                        slot.cancel.notify_waiters();
                        slot.media.take();
                        slot.snapshot.phase = CallPhase::Ended;
                    }
                    _ => {}
                }
                return;
            }
            if !slot.snapshot.phase.finished()
                && (slot.snapshot.phase != CallPhase::Incoming
                    || slot.started.elapsed().as_secs() < 45)
            {
                return;
            }
        }
        let CallAction::Offer {
            is_video,
            group_jid: None,
            ..
        } = incoming.action
        else {
            return;
        };
        if incoming.offline
            || (chrono::Utc::now() - incoming.timestamp)
                .num_seconds()
                .abs()
                > 45
        {
            return;
        }
        let chat = incoming.from.to_non_ad_string();
        if !direct_chat(&chat) {
            return;
        }
        let snapshot = CallSnapshot {
            id: Uuid::new_v4(),
            encryption: Encryption::EndToEnd,
            kind: if is_video {
                CallKind::Video
            } else {
                CallKind::Voice
            },
            phase: CallPhase::Incoming,
            muted: false,
            camera_on: false,
            preview_png: None,
        };
        state.slot = Some(Slot {
            chat: chat.clone(),
            snapshot,
            handle: None,
            incoming: Some(incoming.as_ref().clone()),
            media: None,
            started: Instant::now(),
            cancel: Arc::new(Notify::new()),
            cancelled: Arc::new(AtomicBool::new(false)),
            accepted: false,
            devices: None,
        });
        drop(state);
        let mut cache = cache.lock().await;
        let key = RoomKey::new(Uuid::nil(), &chat);
        if cache.store.room(&key).is_none() {
            cache.store.conversations.push(Conversation::empty(
                key,
                incoming
                    .notify
                    .as_deref()
                    .unwrap_or(&chat)
                    .chars()
                    .take(200)
                    .collect::<String>(),
                RoomKind::Direct,
                Encryption::EndToEnd,
                super::capabilities_for(&chat),
            ));
            let _ = cache.persist().await;
        }
    }

    pub async fn control(
        self: &Arc<Self>,
        client: &Arc<Client>,
        request: CallRequest,
    ) -> anyhow::Result<CallSnapshot> {
        let id = request.command.id();
        if matches!(request.command, CallCommand::End { .. }) {
            return self.end(client, id, &request.chat_id).await;
        }
        if let CallCommand::Start {
            kind,
            devices,
            expires_at,
            ..
        } = request.command
        {
            anyhow::ensure!(
                !self.setup_pending.load(Ordering::Acquire),
                "Call setup is settling"
            );
            let now = chrono::Utc::now().timestamp();
            anyhow::ensure!(
                expires_at >= now && expires_at <= now + 60 && direct_chat(&request.chat_id),
                "Expired or unsupported call request"
            );
            let mut state = self.state.lock().await;
            anyhow::ensure!(
                state
                    .blocked_until
                    .is_none_or(|until| Instant::now() >= until),
                "Call setup is settling"
            );
            state
                .retired
                .retain(|(_, time)| time.elapsed().as_secs() < 65);
            anyhow::ensure!(
                !state.retired.iter().any(|(old, _)| *old == id),
                "This call was cancelled"
            );
            if let Some(slot) = &state.slot {
                if slot.snapshot.id == id {
                    anyhow::ensure!(slot.chat == request.chat_id, "Wrong room");
                    return Ok(slot.snapshot.clone());
                }
                anyhow::ensure!(slot.snapshot.phase.finished(), "A call is already active");
            }
            state.slot = Some(Slot {
                chat: request.chat_id.clone(),
                snapshot: CallSnapshot {
                    id,
                    kind,
                    encryption: Encryption::EndToEnd,
                    phase: CallPhase::Calling,
                    muted: false,
                    camera_on: kind == CallKind::Video,
                    preview_png: None,
                },
                handle: None,
                incoming: None,
                media: None,
                started: Instant::now(),
                cancel: Arc::new(Notify::new()),
                cancelled: Arc::new(AtomicBool::new(false)),
                accepted: false,
                devices: Some(devices.clone()),
            });
            drop(state);
            return self.start(client, id, &request.chat_id, devices).await;
        }
        let mut state = self.state.lock().await;
        let slot = state
            .slot
            .as_mut()
            .filter(|s| s.snapshot.id == id && s.chat == request.chat_id)
            .ok_or_else(|| anyhow::anyhow!("This call is no longer current"))?;
        match request.command {
            CallCommand::Accept { devices, .. } => {
                anyhow::ensure!(
                    slot.snapshot.phase == CallPhase::Incoming
                        && slot.started.elapsed().as_secs() < 45,
                    "The incoming call has expired"
                );
                slot.snapshot.phase = CallPhase::Connecting;
                slot.snapshot.camera_on = slot.snapshot.kind == CallKind::Video;
                slot.accepted = true;
                slot.devices = Some(devices.clone());
                drop(state);
                self.start(client, id, &request.chat_id, devices).await
            }
            CallCommand::Mute { muted, .. } => {
                if slot.snapshot.muted == muted {
                    return Ok(slot.snapshot.clone());
                }
                let handle = slot
                    .handle
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("Call is not connected"))?;
                drop(state);
                handle.set_muted(muted).await?;
                let mut state = self.state.lock().await;
                let slot = state
                    .slot
                    .as_mut()
                    .filter(|s| s.snapshot.id == id)
                    .ok_or_else(|| anyhow::anyhow!("Call ended"))?;
                slot.snapshot.muted = muted;
                Ok(slot.snapshot.clone())
            }
            CallCommand::Camera { enabled, .. } => {
                if slot.snapshot.camera_on == enabled {
                    return Ok(slot.snapshot.clone());
                }
                anyhow::ensure!(slot.snapshot.kind == CallKind::Video, "Not a video call");
                let handle = slot
                    .handle
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("Call is not connected"))?;
                let media = slot
                    .media
                    .as_mut()
                    .ok_or_else(|| anyhow::anyhow!("Call media has ended"))?;
                let ports = if enabled {
                    let devices = slot
                        .devices
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("Choose a camera first"))?;
                    let source = media.camera(devices)?;
                    let sink = media
                        .video
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("Video is unavailable"))?
                        .1
                        .clone();
                    Some((source, sink))
                } else {
                    media.stop_camera();
                    None
                };
                drop(state);
                if let Some((source, sink)) = &ports {
                    handle.resume_video(source.clone(), sink.clone()).await?;
                } else {
                    handle.stop_video().await?;
                }
                let mut state = self.state.lock().await;
                let slot = state
                    .slot
                    .as_mut()
                    .filter(|s| s.snapshot.id == id && !s.snapshot.phase.finished())
                    .ok_or_else(|| anyhow::anyhow!("Call ended"))?;
                if let Some(ports) = ports
                    && let Some(media) = slot.media.as_mut()
                {
                    media.video = Some(ports);
                }
                slot.snapshot.camera_on = enabled;
                Ok(slot.snapshot.clone())
            }
            CallCommand::Poll { .. } => {
                if slot.snapshot.phase == CallPhase::Incoming
                    && slot.started.elapsed().as_secs() >= 45
                {
                    slot.snapshot.phase = CallPhase::Ended;
                    slot.incoming.take();
                }
                if slot
                    .handle
                    .as_ref()
                    .is_some_and(|h| h.media_stats().rtp_received > 0)
                    && !slot.snapshot.phase.finished()
                {
                    slot.snapshot.phase = CallPhase::Connected;
                }
                if let Some(media) = &slot.media {
                    if media.failed.load(Ordering::Acquire) {
                        let snapshot = slot.snapshot.clone();
                        drop(state);
                        let _ = self.end(client, id, &request.chat_id).await;
                        return Ok(CallSnapshot {
                            phase: CallPhase::Failed,
                            ..snapshot
                        });
                    }
                    slot.snapshot.preview_png = media
                        .preview
                        .lock()
                        .ok()
                        .and_then(|p| p.as_ref().map(|bytes| STANDARD.encode(bytes)));
                }
                Ok(slot.snapshot.clone())
            }
            _ => anyhow::bail!("Unsupported call operation"),
        }
    }

    async fn start(
        self: &Arc<Self>,
        client: &Arc<Client>,
        id: Uuid,
        chat: &str,
        devices: messages_media::Selection,
    ) -> anyhow::Result<CallSnapshot> {
        let state = self.state.lock().await;
        let slot = state
            .slot
            .as_ref()
            .filter(|s| s.snapshot.id == id && !s.cancelled.load(Ordering::Acquire))
            .ok_or_else(|| anyhow::anyhow!("Call cancelled"))?;
        let incoming = slot.incoming.clone();
        let kind = slot.snapshot.kind;
        let cancel = slot.cancel.clone();
        let cancelled = slot.cancelled.clone();
        drop(state);
        anyhow::ensure!(
            !self.setup_pending.swap(true, Ordering::AcqRel),
            "Call setup is settling"
        );
        let guard = SetupGuard(self.setup_pending.clone());
        let media = tokio::select! {
            _ = cancel.notified() => Err(anyhow::anyhow!("Call cancelled")),
            result = Media::open(&devices, kind == CallKind::Video) => result,
        }?;
        let microphone = media.microphone.clone();
        let speaker = media.speaker.clone();
        let video = media.video.clone();
        {
            let mut state = self.state.lock().await;
            let slot = state
                .slot
                .as_mut()
                .filter(|s| s.snapshot.id == id && !s.cancelled.load(Ordering::Acquire))
                .ok_or_else(|| anyhow::anyhow!("Call cancelled"))?;
            slot.media = Some(media);
        }
        let client = client.clone();
        let chat = chat.to_owned();
        let cancelled_setup = cancelled.clone();
        // The SDK offer write is not cancellation-safe; keep one setup alive to retire its late handle.
        let mut setup = tokio::spawn(async move {
            let _guard = guard;
            let voip = client.voip();
            let handle = if let Some(incoming) = incoming {
                let mut builder = voip.accept(&incoming).audio(microphone, speaker.clone());
                if let Some((source, sink)) = video {
                    builder = builder.video(source, sink);
                }
                builder.start().await?
            } else {
                let peer: Jid = chat.parse()?;
                let mut builder = voip.call(&peer).audio(microphone, speaker.clone());
                if let Some((source, sink)) = video {
                    builder = builder.video(source, sink);
                }
                builder.start().await?
            };
            if cancelled_setup.load(Ordering::Acquire) {
                let _ = tokio::time::timeout(std::time::Duration::from_secs(3), handle.terminate())
                    .await;
                handle.hangup_local().await;
                anyhow::bail!("Call cancelled");
            }
            Ok::<_, anyhow::Error>(SetupResult {
                handle: Some(handle),
                speaker,
            })
        });
        let result = tokio::select! {
            _ = cancel.notified() => Err(anyhow::anyhow!("Call cancelled")),
            result = tokio::time::timeout(std::time::Duration::from_secs(25), &mut setup) =>
                result.map_err(|_| anyhow::anyhow!("Call setup timed out"))
                .and_then(|r| r.map_err(anyhow::Error::from)).and_then(|r| r),
        };
        let mut owned = match result {
            Ok(value) => value,
            Err(error) => {
                cancelled.store(true, Ordering::Release);
                let mut state = self.state.lock().await;
                if let Some(slot) = state.slot.as_mut().filter(|s| s.snapshot.id == id) {
                    slot.media.take();
                    if !slot.snapshot.phase.finished() {
                        slot.snapshot.phase = CallPhase::Failed;
                    }
                }
                return Err(error);
            }
        };
        let handle = owned.handle.as_ref().unwrap().clone();
        let speaker = owned.speaker.clone();
        if cancelled.load(Ordering::Acquire) {
            handle.terminate().await;
            anyhow::bail!("Call cancelled");
        }
        let mut events = handle.take_events();
        let mut state = self.state.lock().await;
        let Some(slot) = state
            .slot
            .as_mut()
            .filter(|s| s.snapshot.id == id && !s.cancelled.load(Ordering::Acquire))
        else {
            drop(state);
            handle.terminate().await;
            anyhow::bail!("Call cancelled");
        };
        slot.handle = Some(handle.clone());
        owned.handle.take();
        let snapshot = slot.snapshot.clone();
        drop(state);
        let weak = Arc::downgrade(self);
        let end_handle = handle.clone();
        tokio::spawn(async move {
            end_handle.wait_ended().await;
            finish(&weak, id, CallPhase::Ended).await;
        });
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut opus = whatsapp_rust::voip::audio::WaOpusDecoder::new().ok();
            if let Some(events) = events.as_mut() {
                while let Ok(event) = events.recv().await {
                    match event {
                        CallEvent::RelayAllocated => {
                            if let Some(manager) = weak.upgrade() {
                                let mut state = manager.state.lock().await;
                                if let Some(slot) = state.slot.as_mut().filter(|s| {
                                    s.snapshot.id == id
                                        && s.accepted
                                        && !s.snapshot.phase.finished()
                                }) {
                                    slot.snapshot.phase = CallPhase::Connected;
                                }
                            }
                        }
                        CallEvent::ForeignAudio(payload) => {
                            if let Some(decoder) = opus.as_mut()
                                && let Ok(pcm) = decoder.decode_mlow_escape(&payload)
                            {
                                let _ = speaker.try_send(pcm.to_vec());
                            }
                        }
                        CallEvent::RelayAllocateFailed(_)
                        | CallEvent::RelayAllocateTimedOut
                        | CallEvent::RelayReconnectTimedOut
                        | CallEvent::MediaSetupFailed(_)
                        | CallEvent::AudioFormatMismatch { .. } => {
                            finish(&weak, id, CallPhase::Failed).await;
                            handle.terminate().await;
                            break;
                        }
                        _ => {}
                    }
                }
            }
        });
        Ok(snapshot)
    }

    async fn end(
        &self,
        client: &Arc<Client>,
        id: Uuid,
        chat: &str,
    ) -> anyhow::Result<CallSnapshot> {
        let mut state = self.state.lock().await;
        if let Some(slot) = state.slot.as_ref().filter(|s| s.snapshot.id == id) {
            anyhow::ensure!(slot.chat == chat, "Wrong call room");
        }
        state
            .retired
            .retain(|(_, time)| time.elapsed().as_secs() < 65);
        if state.retired.len() >= 256 && !state.retired.iter().any(|(old, _)| *old == id) {
            // Delay new calls beyond their setup deadline; ending media stays unconditional.
            state.blocked_until = Some(Instant::now() + std::time::Duration::from_secs(65));
            state.retired.pop_front();
        }
        if !state.retired.iter().any(|(old, _)| *old == id) {
            state.retired.push_back((id, Instant::now()));
        }
        let Some(slot) = state.slot.as_mut().filter(|s| s.snapshot.id == id) else {
            return Ok(CallSnapshot {
                id,
                kind: CallKind::Voice,
                encryption: Encryption::EndToEnd,
                phase: CallPhase::Ended,
                muted: false,
                camera_on: false,
                preview_png: None,
            });
        };
        anyhow::ensure!(slot.chat == chat, "Wrong call room");
        slot.cancelled.store(true, Ordering::Release);
        slot.cancel.notify_waiters();
        slot.media.take();
        slot.snapshot.phase = CallPhase::Ended;
        slot.snapshot.preview_png = None;
        let snapshot = slot.snapshot.clone();
        let handle = slot.handle.take();
        let incoming = slot.incoming.take();
        drop(state);
        if let Some(handle) = handle {
            let _ =
                tokio::time::timeout(std::time::Duration::from_secs(3), handle.terminate()).await;
            handle.hangup_local().await;
        } else if let Some(incoming) = incoming {
            let _ = client.voip().reject(&incoming).await;
        }
        Ok(snapshot)
    }
}

struct SetupResult {
    handle: Option<CallHandle>,
    speaker: async_channel::Sender<Vec<i16>>,
}
impl Drop for SetupResult {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            tokio::spawn(async move {
                let _ = tokio::time::timeout(std::time::Duration::from_secs(3), handle.terminate())
                    .await;
                handle.hangup_local().await;
            });
        }
    }
}

struct SetupGuard(Arc<AtomicBool>);
impl Drop for SetupGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

async fn finish(weak: &Weak<Manager>, id: Uuid, phase: CallPhase) {
    if let Some(manager) = weak.upgrade() {
        let mut state = manager.state.lock().await;
        if let Some(slot) = state.slot.as_mut().filter(|s| s.snapshot.id == id) {
            if !slot.snapshot.phase.finished() {
                slot.snapshot.phase = phase;
            }
            slot.media.take();
            slot.snapshot.preview_png = None;
        }
    }
}
fn direct_chat(chat: &str) -> bool {
    chat.len() <= 256
        && (chat.ends_with("@s.whatsapp.net") || chat.ends_with("@lid"))
        && !chat.contains(['/', '\n', '\r'])
}

#[cfg(all(test, feature = "call-fixtures"))]
mod signaling_tests {
    use super::*;
    use whatsapp_rust::test_support::CallFixture;

    #[tokio::test]
    async fn end_during_offer_barrier_cancels_sdk_registration_and_capture() {
        assert!(messages_media::synthetic());
        let fixture = CallFixture::new().await.unwrap();
        let manager = Arc::new(Manager::default());
        let id = Uuid::new_v4();
        let chat = fixture.peer().to_string();
        let target = manager.clone();
        let client = fixture.client().clone();
        let room = chat.clone();
        let started = tokio::spawn(async move {
            target
                .control(
                    &client,
                    CallRequest {
                        chat_id: room,
                        command: CallCommand::Start {
                            id,
                            kind: CallKind::Voice,
                            devices: messages_media::Selection {
                                microphone: "test-input".into(),
                                speaker: "test-output".into(),
                                camera: "test-camera".into(),
                            },
                            expires_at: chrono::Utc::now().timestamp() + 30,
                        },
                    },
                )
                .await
        });
        let offer = fixture.next_offer().await.unwrap();
        let sdk_id = offer
            .stanza()
            .get_optional_child("offer")
            .unwrap()
            .attrs()
            .optional_string("call-id")
            .unwrap()
            .into_owned();
        let ended = manager
            .control(
                fixture.client(),
                CallRequest {
                    chat_id: chat,
                    command: CallCommand::End { id },
                },
            )
            .await
            .unwrap();
        assert_eq!(ended.phase, CallPhase::Ended);
        assert!(started.await.unwrap().is_err());
        assert!(manager.setup_pending.load(Ordering::Acquire));
        offer.complete().unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while manager.setup_pending.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(fixture.call_snapshot(&sdk_id).is_none());
        assert!(
            manager
                .state
                .lock()
                .await
                .slot
                .as_ref()
                .unwrap()
                .media
                .is_none()
        );
        fixture.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn voice_and_video_offers_use_the_real_sdk_and_end_retires_local_media() {
        assert!(
            messages_media::synthetic(),
            "Run call fixtures with MESSAGES_MEDIA_TEST=1"
        );
        for kind in [CallKind::Voice, CallKind::Video] {
            let fixture = CallFixture::new().await.unwrap();
            let manager = Arc::new(Manager::default());
            let id = Uuid::new_v4();
            assert!(
                manager
                    .control(
                        fixture.client(),
                        CallRequest {
                            chat_id: "123@g.us".into(),
                            command: CallCommand::Start {
                                id,
                                kind,
                                devices: messages_media::Selection {
                                    microphone: "test-input".into(),
                                    speaker: "test-output".into(),
                                    camera: "test-camera".into(),
                                },
                                expires_at: chrono::Utc::now().timestamp() + 30
                            }
                        }
                    )
                    .await
                    .is_err()
            );
            let chat = fixture.peer().to_string();
            let target = manager.clone();
            let client = fixture.client().clone();
            let room = chat.clone();
            let started = tokio::spawn(async move {
                target
                    .control(
                        &client,
                        CallRequest {
                            chat_id: room,
                            command: CallCommand::Start {
                                id,
                                kind,
                                devices: messages_media::Selection {
                                    microphone: "test-input".into(),
                                    speaker: "test-output".into(),
                                    camera: "test-camera".into(),
                                },
                                expires_at: chrono::Utc::now().timestamp() + 30,
                            },
                        },
                    )
                    .await
            });
            let offer = fixture.next_offer().await.unwrap();
            let node = offer.stanza().get_optional_child("offer").unwrap();
            assert_eq!(
                node.get_optional_child("video").is_some(),
                kind == CallKind::Video
            );
            let sdk_id = node
                .attrs()
                .optional_string("call-id")
                .unwrap()
                .into_owned();
            offer.complete().unwrap();
            let snapshot = started.await.unwrap().unwrap();
            assert_eq!(snapshot.phase, CallPhase::Calling);
            assert_eq!(snapshot.encryption, Encryption::EndToEnd);
            assert!(fixture.call_snapshot(&sdk_id).is_some());
            let end = manager
                .control(
                    fixture.client(),
                    CallRequest {
                        chat_id: chat.clone(),
                        command: CallCommand::End { id },
                    },
                )
                .await
                .unwrap();
            assert_eq!(end.phase, CallPhase::Ended);
            assert!(fixture.call_snapshot(&sdk_id).is_none());
            assert!(
                manager
                    .state
                    .lock()
                    .await
                    .slot
                    .as_ref()
                    .unwrap()
                    .media
                    .is_none()
            );
            let result = manager
                .control(
                    fixture.client(),
                    CallRequest {
                        chat_id: chat,
                        command: CallCommand::Start {
                            id,
                            kind,
                            devices: Default::default(),
                            expires_at: chrono::Utc::now().timestamp() + 30,
                        },
                    },
                )
                .await;
            assert!(result.is_err());
            fixture.shutdown().await.unwrap();
        }
    }
}
