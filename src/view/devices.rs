use super::*;
use crate::app::devices::{Event, Kind};
use enzui_iced_widgets::{combo_box::enz_combo_box, preferences_row::enz_preferences_row};
use iced::widget::column;

pub(super) fn view(app: &App) -> Element<'_, Message> {
    let state = &app.devices;
    let mut body = column![enz_text("Devices").size(TextSize::Headline), hint("Choose devices for calls and try them locally. Test recordings stay in memory and are discarded when you leave this page.")].spacing(12);
    for (label, kind, list, selected) in [
        (
            "Microphone",
            Kind::Microphone,
            &state.list.microphones,
            &state.selection.microphone,
        ),
        (
            "Speaker",
            Kind::Speaker,
            &state.list.speakers,
            &state.selection.speaker,
        ),
        (
            "Camera",
            Kind::Camera,
            &state.list.cameras,
            &state.selection.camera,
        ),
    ] {
        if list.is_empty() {
            body = body.push(hint(format!("No {} available", label.to_lowercase())));
            continue;
        }
        let names = list.iter().map(|d| d.name.clone()).collect();
        let current = list
            .iter()
            .find(|d| &d.id == selected)
            .map(|d| d.name.clone());
        body = body.push(enz_combo_box(label, names, current, move |name| {
            let id = list
                .iter()
                .find(|d| d.name == name)
                .map(|d| d.id.clone())
                .unwrap_or_default();
            Message::Devices(Event::Select(kind, id))
        }));
    }
    body = body.push(
        enz_preferences_row("Camera preview")
            .switch(state.camera_on, |on| Message::Devices(Event::Camera(on)))
            .enabled(
                !state.list.cameras.is_empty()
                    && state.list.capture_available
                    && (!state.busy || state.camera_on),
            )
            .id_prefix("messages-camera-preview"),
    );
    if let Some(frame) = &state.frame {
        body = body.push(
            enzui_iced_widgets::image::enz_image_handle(frame.clone())
                .fit(enzui_iced_widgets::image::EnzImageFit::Contain)
                .alt_text("Live camera preview")
                .width(Length::Fill)
                .height(Length::Fixed(180.0))
                .into_element(),
        );
    }
    body = body.push(
        row![
            enz_button(
                if state.recording {
                    "Recording…"
                } else {
                    "Record 5 seconds"
                },
                Message::Devices(Event::Record)
            )
            .id(WidgetId::new("messages-devices-record"))
            .icon("mic")
            .kind(ButtonKind::Secondary)
            .size(ButtonSize::Sm)
            .enabled(
                !state.busy
                    && !state.camera_on
                    && !state.list.microphones.is_empty()
                    && state.list.capture_available
            ),
            enz_button("Play recording", Message::Devices(Event::Play))
                .id(WidgetId::new("messages-devices-play"))
                .icon("play")
                .kind(ButtonKind::Secondary)
                .size(ButtonSize::Sm)
                .enabled(!state.busy && state.recording_bytes.is_some()),
        ]
        .spacing(8)
        .wrap(),
    );
    body = body.push(hint(&state.status));
    body.push(
        enz_button("Refresh devices", Message::Devices(Event::Refresh))
            .id(WidgetId::new("messages-devices-refresh"))
            .kind(ButtonKind::Quiet)
            .size(ButtonSize::Sm)
            .enabled(!state.busy && !state.camera_on),
    )
    .into()
}
