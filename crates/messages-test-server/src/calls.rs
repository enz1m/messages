use crate::{MockState, reply};
use axum::response::Response;
use messages_core::{Capabilities, Capability};
use messages_protocols::calls::{CallCommand, CallPhase, CallRequest, CallSnapshot};
use serde_json::{Value, json};

pub fn capabilities(base: &Capabilities) -> Capabilities {
    Capabilities::new(Capability::ALL.into_iter().filter(|c| {
        base.supports(*c) || matches!(c, Capability::VoiceCall | Capability::VideoCall)
    }))
}

pub fn handle(state: &mut MockState, body: Value) -> Response {
    if !state.calls_enabled {
        return reply(
            501,
            json!({"error":"Calls are not enabled in this fixture"}),
        );
    }
    let Ok(request) = serde_json::from_value::<CallRequest>(body) else {
        return reply(400, json!({"error":"Invalid call request"}));
    };
    let id = request.command.id();
    if let Some((chat, _)) = state.call_states.get(&id)
        && chat != &request.chat_id
    {
        return reply(403, json!({"error":"Wrong room"}));
    }
    if let CallCommand::Start {
        kind, expires_at, ..
    } = request.command
    {
        if chrono::Utc::now().timestamp() > expires_at {
            return reply(400, json!({"error":"Expired call"}));
        }
        let preview = state.call_preview.clone();
        let (_, call) = state.call_states.entry(id).or_insert_with(|| {
            (
                request.chat_id,
                CallSnapshot {
                    id,
                    kind,
                    encryption: messages_core::Encryption::Sample,
                    phase: CallPhase::Connected,
                    muted: false,
                    camera_on: kind == messages_protocols::calls::CallKind::Video,
                    preview_png: preview,
                },
            )
        });
        return reply(200, json!(call));
    }
    let Some((_, call)) = state.call_states.get_mut(&id) else {
        return reply(404, json!({"error":"Unknown call"}));
    };
    match request.command {
        CallCommand::Mute { muted, .. } if !call.phase.finished() => call.muted = muted,
        CallCommand::Camera { enabled, .. } if !call.phase.finished() => call.camera_on = enabled,
        CallCommand::End { .. } => {
            call.phase = CallPhase::Ended;
            call.preview_png = None;
        }
        CallCommand::Accept { .. } if call.phase == CallPhase::Incoming => {
            call.phase = CallPhase::Connected
        }
        _ => {}
    }
    reply(200, json!(call))
}
