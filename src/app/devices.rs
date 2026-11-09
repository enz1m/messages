use super::{App, Dialog, Message, jobs};
use iced::Task;
use messages_media::{Devices, Selection};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone, Copy, Debug)]
pub enum Kind {
    Microphone,
    Speaker,
    Camera,
}

#[derive(Clone, Debug)]
pub enum Event {
    Refresh,
    Listed(Uuid, Devices),
    Select(Kind, String),
    Camera(bool),
    FrameTick,
    Frame(Uuid, Result<Vec<u8>, String>),
    Record,
    Recorded(Uuid, Result<Vec<u8>, String>),
    Play,
    Played(Uuid, Result<(), String>),
}

pub struct State {
    pub list: Devices,
    pub selection: Selection,
    pub camera_on: bool,
    pub busy: bool,
    pub recording: bool,
    pub status: String,
    pub frame: Option<iced::widget::image::Handle>,
    pub recording_bytes: Option<Arc<Vec<u8>>>,
    generation: Uuid,
    job: Option<iced::task::Handle>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            list: Devices::default(),
            selection: Selection::default(),
            camera_on: false,
            busy: false,
            recording: false,
            status: String::new(),
            frame: None,
            recording_bytes: None,
            generation: Uuid::new_v4(),
            job: None,
        }
    }
}

impl State {
    pub fn stop(&mut self) {
        self.generation = Uuid::new_v4();
        self.job.take();
        self.camera_on = false;
        self.busy = false;
        self.recording = false;
        self.frame = None;
        self.recording_bytes = None;
        self.status.clear();
    }
}

impl App {
    pub(super) fn device_event(&mut self, event: Event) -> Task<Message> {
        if self.dialog != Some(Dialog::Settings)
            || self.settings_page != "devices"
            || self.stopping()
        {
            self.devices.stop();
            return Task::none();
        }
        let generation = self.devices.generation;
        let selection = self.devices.selection.clone();
        let task = match event {
            Event::Refresh => {
                self.devices.stop();
                self.devices.selection = self.preferences.media_devices.clone();
                let generation = self.devices.generation;
                self.devices.busy = true;
                self.devices.status = "Looking for devices…".into();
                self.track_job(
                    jobs::Kind::MediaCheck,
                    messages_media::discover(),
                    move |list| Message::Devices(Event::Listed(generation, list)),
                )
            }
            Event::Listed(ticket, list) if ticket == generation => {
                self.devices.job.take();
                self.devices.busy = false;
                let pick = |id: &mut String, devices: &[messages_media::Device]| {
                    if !devices.iter().any(|d| d.id == *id)
                        && let Some(first) = devices.first()
                    {
                        *id = first.id.clone();
                    }
                };
                pick(&mut self.devices.selection.microphone, &list.microphones);
                pick(&mut self.devices.selection.speaker, &list.speakers);
                pick(&mut self.devices.selection.camera, &list.cameras);
                if self.preferences.media_devices != self.devices.selection {
                    self.preferences.media_devices = self.devices.selection.clone();
                    self.changed();
                }
                self.devices.list = list;
                self.devices.status = if self.devices.list.capture_available {
                    "Camera and microphone stay off until you start a check."
                } else {
                    "Install FFmpeg with PulseAudio and camera support to run device checks."
                }
                .into();
                return Task::none();
            }
            Event::Select(kind, id) => {
                let list = match kind {
                    Kind::Microphone => &self.devices.list.microphones,
                    Kind::Speaker => &self.devices.list.speakers,
                    Kind::Camera => &self.devices.list.cameras,
                };
                if !list.iter().any(|d| d.id == id) {
                    return Task::none();
                }
                self.devices.stop();
                match kind {
                    Kind::Microphone => self.devices.selection.microphone = id,
                    Kind::Speaker => self.devices.selection.speaker = id,
                    Kind::Camera => self.devices.selection.camera = id,
                }
                self.preferences.media_devices = self.devices.selection.clone();
                self.changed();
                return Task::none();
            }
            Event::Camera(enabled) => {
                if !enabled {
                    self.devices.stop();
                    self.devices.status = "Camera preview stopped.".into();
                    return Task::none();
                }
                if self.devices.busy || self.devices.list.cameras.is_empty() {
                    return Task::none();
                }
                self.preferences.media_devices = self.devices.selection.clone();
                self.changed();
                self.devices.camera_on = true;
                self.devices.status = "Camera preview is on. Turn it off before recording.".into();
                return self.device_event(Event::FrameTick);
            }
            Event::FrameTick if self.devices.camera_on && !self.devices.busy => {
                self.devices.busy = true;
                self.track_job(
                    jobs::Kind::MediaCheck,
                    async move { messages_media::camera_frame(&selection).await },
                    move |result| Message::Devices(Event::Frame(generation, result)),
                )
            }
            Event::Frame(ticket, result) if ticket == generation && self.devices.camera_on => {
                self.devices.job.take();
                self.devices.busy = false;
                match result {
                    Ok(bytes) => {
                        self.devices.frame = Some(iced::widget::image::Handle::from_bytes(bytes))
                    }
                    Err(error) => {
                        self.devices.camera_on = false;
                        self.devices.frame = None;
                        self.devices.status = error;
                    }
                }
                return Task::none();
            }
            Event::Record if !self.devices.busy && !self.devices.camera_on => {
                self.devices.busy = true;
                self.preferences.media_devices = self.devices.selection.clone();
                self.changed();
                self.devices.recording = true;
                self.devices.recording_bytes = None;
                self.devices.status = "Recording a five-second microphone test…".into();
                self.track_job(
                    jobs::Kind::MediaCheck,
                    async move { messages_media::record_test(&selection).await },
                    move |result| Message::Devices(Event::Recorded(generation, result)),
                )
            }
            Event::Recorded(ticket, result) if ticket == generation => {
                self.devices.job.take();
                self.devices.busy = false;
                self.devices.recording = false;
                match result {
                    Ok(bytes) => {
                        self.devices.recording_bytes = Some(Arc::new(bytes));
                        self.devices.status =
                            "Recording ready. Play it through the selected speaker.".into();
                    }
                    Err(error) => self.devices.status = error,
                }
                return Task::none();
            }
            Event::Play if !self.devices.busy => {
                let Some(bytes) = self.devices.recording_bytes.clone() else {
                    return Task::none();
                };
                self.devices.busy = true;
                self.devices.status = "Playing the microphone test…".into();
                self.track_job(jobs::Kind::MediaCheck, async move { messages_media::play_test(&selection, bytes.as_ref().clone()).await }, move |result| Message::Devices(Event::Played(generation, result)))
            }
            Event::Played(ticket, result) if ticket == generation => {
                self.devices.job.take();
                self.devices.busy = false;
                self.devices.status = result.err().unwrap_or_else(|| "Playback finished.".into());
                return Task::none();
            }
            _ => return Task::none(),
        };
        let (task, handle) = task.abortable();
        self.devices.job = Some(handle.abort_on_drop());
        task
    }
}
