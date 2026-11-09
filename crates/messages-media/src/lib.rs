//! Bounded local device checks and media subprocesses; capture starts only on request.

mod process;
pub use process::{audio_input, audio_output, camera_stream, decode_video, read_png};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio::process::Command;

#[derive(Clone, Debug, Default, Serialize, Deserialize, Eq, PartialEq)]
pub struct Device {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Devices {
    pub microphones: Vec<Device>,
    pub speakers: Vec<Device>,
    pub cameras: Vec<Device>,
    pub capture_available: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(default)]
pub struct Selection {
    pub microphone: String,
    pub speaker: String,
    pub camera: String,
}

impl Default for Selection {
    fn default() -> Self {
        Self {
            microphone: "default".into(),
            speaker: "default".into(),
            camera: String::new(),
        }
    }
}

pub fn synthetic() -> bool {
    std::env::var("MESSAGES_MEDIA_TEST").as_deref() == Ok("1")
}

fn safe_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256 && !id.contains(['\0', '\n', '\r'])
}

pub fn valid_camera(id: &str) -> bool {
    id.strip_prefix("/dev/video")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()))
        || (synthetic() && id == "test-camera")
}

pub fn validate_selection(s: &Selection, video: bool) -> Result<(), String> {
    if !safe_id(&s.microphone) || !safe_id(&s.speaker) || (video && !valid_camera(&s.camera)) {
        return Err("Select an available microphone, speaker and camera in Devices.".into());
    }
    Ok(())
}

pub async fn discover() -> Devices {
    if synthetic() {
        let device = |id: &str, name: &str| Device {
            id: id.into(),
            name: name.into(),
        };
        return Devices {
            microphones: vec![device("test-input", "Synthetic microphone")],
            speakers: vec![device("test-output", "Synthetic speaker")],
            cameras: vec![device("test-camera", "Synthetic camera")],
            capture_available: true,
        };
    }
    let mut devices = Devices {
        capture_available: process::output(Command::new("ffmpeg").arg("-version"), 32768, 3)
            .await
            .is_ok(),
        ..Devices::default()
    };
    for (kind, list) in [
        ("sources", &mut devices.microphones),
        ("sinks", &mut devices.speakers),
    ] {
        let mut command = Command::new("pactl");
        command.args(["--format=json", "list", kind]);
        if let Ok(bytes) = process::output(&mut command, 2 * 1024 * 1024, 3).await
            && let Ok(serde_json::Value::Array(values)) = serde_json::from_slice(&bytes)
        {
            for value in values.iter().take(64) {
                if let Some(id) = value["name"]
                    .as_str()
                    .filter(|id| safe_id(id) && !id.ends_with(".monitor"))
                {
                    list.push(Device {
                        id: id.into(),
                        name: value["description"]
                            .as_str()
                            .unwrap_or(id)
                            .chars()
                            .take(180)
                            .collect(),
                    });
                }
            }
        }
    }
    for (name, list) in [
        ("System default microphone", &mut devices.microphones),
        ("System default speaker", &mut devices.speakers),
    ] {
        if !list.is_empty() {
            list.insert(
                0,
                Device {
                    id: "default".into(),
                    name: name.into(),
                },
            );
        }
        let mut seen = std::collections::BTreeMap::<String, usize>::new();
        for device in list.iter_mut() {
            let count = seen.entry(device.name.clone()).or_default();
            *count += 1;
            if *count > 1 {
                device.name = format!("{} ({count})", device.name);
            }
        }
    }
    if let Ok(mut entries) = tokio::fs::read_dir("/sys/class/video4linux").await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let id = format!("/dev/{}", entry.file_name().to_string_lossy());
            if valid_camera(&id) && Path::new(&id).exists() {
                let name = tokio::fs::read_to_string(entry.path().join("name"))
                    .await
                    .unwrap_or_else(|_| id.clone());
                devices.cameras.push(Device {
                    id,
                    name: name.trim().chars().take(180).collect(),
                });
            }
            if devices.cameras.len() == 32 {
                break;
            }
        }
    }
    devices
}

pub async fn camera_frame(selection: &Selection) -> Result<Vec<u8>, String> {
    validate_selection(selection, true)?;
    let mut cmd = process::camera_command(&selection.camera)?;
    cmd.args([
        "-frames:v",
        "1",
        "-vf",
        "scale=640:480:force_original_aspect_ratio=decrease",
        "-f",
        "image2pipe",
        "-vcodec",
        "png",
        "-threads",
        "2",
        "pipe:1",
    ]);
    process::output(&mut cmd, 4 * 1024 * 1024, 6).await
}

pub async fn record_test(selection: &Selection) -> Result<Vec<u8>, String> {
    validate_selection(selection, false)?;
    let mut cmd = process::microphone_command(&selection.microphone);
    cmd.args(["-t", "5", "-ac", "1", "-ar", "16000", "-f", "wav", "pipe:1"]);
    process::output(&mut cmd, 512 * 1024, 8).await
}

pub async fn play_test(selection: &Selection, audio: Vec<u8>) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;
    validate_selection(selection, false)?;
    if audio.len() > 512 * 1024 {
        return Err("The test recording is too large.".into());
    }
    let mut cmd = process::ffmpeg();
    cmd.args(["-re", "-i", "pipe:0"]);
    process::speaker_args(&mut cmd, &selection.speaker);
    let mut child = process::spawn(&mut cmd, true)?;
    let mut input = child.stdin.take().ok_or("Audio input unavailable")?;
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        input
            .write_all(&audio)
            .await
            .map_err(|_| "Could not play the test recording.")?;
        drop(input);
        child
            .wait()
            .await
            .map_err(|_| "Audio playback failed.")
            .and_then(|s| {
                if s.success() {
                    Ok(())
                } else {
                    Err("Could not play through the selected speaker.")
                }
            })
    })
    .await
    .map_err(|_| "Audio playback timed out.".to_string())?
    .map_err(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn camera_paths_and_device_names_are_bounded() {
        assert!(valid_camera("/dev/video12"));
        for path in [
            "/dev/video",
            "/dev/video0/../../etc/passwd",
            "https://camera",
            "/tmp/camera",
        ] {
            assert!(!valid_camera(path));
        }
        let selection = Selection {
            microphone: "bad\nname".into(),
            ..Selection::default()
        };
        assert!(validate_selection(&selection, false).is_err());
        assert!(validate_selection(&Selection::default(), true).is_err());
    }
}
