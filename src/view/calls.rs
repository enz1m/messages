use super::*;
use crate::app::calls::Event;
use iced::widget::column;
use messages_protocols::calls::{CallKind, CallPhase};

pub(super) fn view(app: &App) -> Element<'_, Message> {
    let Some(call) = app.calls.active.as_ref() else {
        return hint("Call ended.");
    };
    let title = app
        .store
        .room(&call.room)
        .map(|r| r.title.as_str())
        .unwrap_or("Call");
    let mut body = column![
        enz_text(title).size(TextSize::Headline),
        hint(format!(
            "{} · {}",
            if call.snapshot.kind == CallKind::Video {
                "Video call"
            } else {
                "Voice call"
            },
            call.snapshot.encryption.label()
        )),
        enz_text(call.snapshot.phase.label()).size(TextSize::Body),
    ]
    .spacing(12);
    if let Some(preview) = &call.preview {
        body = body.push(
            enzui_iced_widgets::image::enz_image_handle(preview.clone())
                .fit(enzui_iced_widgets::image::EnzImageFit::Contain)
                .alt_text("Call video")
                .width(Length::Fill)
                .height(Length::Fixed(240.0))
                .into_element(),
        );
    }
    if let Some(error) = &call.error {
        body = body.push(hint(error));
    }
    let mut actions = row![].spacing(8);
    if call.snapshot.phase == CallPhase::Incoming {
        actions = actions.push(
            enz_button(
                "Answer",
                Message::Call(Event::Accept(call.snapshot.id, call.session)),
            )
            .id(WidgetId::new("messages-call-answer"))
            .icon("phone")
            .size(ButtonSize::Sm)
            .enabled(!call.busy || call.polling),
        );
    } else if !call.snapshot.phase.finished() {
        actions = actions.push(
            enz_button(
                if call.snapshot.muted {
                    "Unmute"
                } else {
                    "Mute"
                },
                Message::Call(Event::Mute(
                    call.snapshot.id,
                    call.session,
                    !call.snapshot.muted,
                )),
            )
            .id(WidgetId::new("messages-call-mute"))
            .icon(if call.snapshot.muted {
                "mic-off"
            } else {
                "mic"
            })
            .kind(ButtonKind::Secondary)
            .size(ButtonSize::Sm)
            .enabled(!call.busy || call.polling),
        );
        if call.snapshot.kind == CallKind::Video {
            actions = actions.push(
                enz_button(
                    if call.snapshot.camera_on {
                        "Stop camera"
                    } else {
                        "Start camera"
                    },
                    Message::Call(Event::Camera(
                        call.snapshot.id,
                        call.session,
                        !call.snapshot.camera_on,
                    )),
                )
                .id(WidgetId::new("messages-call-camera"))
                .icon(if call.snapshot.camera_on {
                    "camera-off"
                } else {
                    "camera"
                })
                .kind(ButtonKind::Secondary)
                .size(ButtonSize::Sm)
                .enabled(!call.busy || call.polling),
            );
        }
    }
    actions = actions.push(
        enz_button(
            if call.snapshot.phase.finished() {
                "Close"
            } else if call.snapshot.phase == CallPhase::Incoming {
                "Decline"
            } else {
                "End call"
            },
            Message::Call(Event::End(call.snapshot.id, call.session)),
        )
        .id(WidgetId::new("messages-call-end"))
        .icon("phone-off")
        .kind(ButtonKind::Secondary)
        .size(ButtonSize::Sm),
    );
    body.push(actions.wrap()).into()
}
