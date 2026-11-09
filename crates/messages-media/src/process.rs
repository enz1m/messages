use super::{Selection, synthetic, valid_camera};
use std::process::Stdio;
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::{Child, Command},
};

pub(crate) fn ffmpeg() -> Command {
    let mut cmd = Command::new("ffmpeg");
    cmd.args([
        "-nostdin",
        "-hide_banner",
        "-loglevel",
        "error",
        "-threads",
        "2",
        "-max_alloc",
        "67108864",
    ]);
    cmd
}

pub(crate) fn microphone_command(id: &str) -> Command {
    let mut cmd = ffmpeg();
    if synthetic() && id == "test-input" {
        cmd.args([
            "-re",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=16000",
        ]);
    } else {
        cmd.args(["-f", "pulse", "-i", id]);
    }
    cmd
}

pub(crate) fn camera_command(id: &str) -> Result<Command, String> {
    if !valid_camera(id) {
        return Err("Select a camera in Devices first.".into());
    }
    let mut cmd = ffmpeg();
    if synthetic() && id == "test-camera" {
        cmd.args(["-re", "-f", "lavfi", "-i", "testsrc2=size=640x480:rate=15"]);
    } else {
        cmd.args([
            "-f",
            "v4l2",
            "-framerate",
            "15",
            "-video_size",
            "640x480",
            "-i",
            id,
        ]);
    }
    Ok(cmd)
}

pub(crate) fn speaker_args(cmd: &mut Command, id: &str) {
    if synthetic() && id == "test-output" {
        cmd.args(["-f", "null", "-"]);
    } else {
        if id != "default" {
            cmd.args(["-device", id]);
        }
        cmd.args([
            "-f",
            "pulse",
            "-name",
            "Messages",
            "-stream_name",
            "Messages audio",
            "Messages",
        ]);
    }
}

pub(crate) fn spawn(cmd: &mut Command, input: bool) -> Result<Child, String> {
    cmd.kill_on_drop(true)
        .stdin(if input { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| {
            "Media tools are unavailable. Install FFmpeg with PulseAudio and camera support.".into()
        })
}

pub(crate) async fn output(cmd: &mut Command, limit: u64, seconds: u64) -> Result<Vec<u8>, String> {
    let mut child = spawn(cmd, false)?;
    let mut stdout = child.stdout.take().ok_or("Media output unavailable")?;
    tokio::time::timeout(std::time::Duration::from_secs(seconds), async {
        let mut bytes = Vec::new();
        (&mut stdout)
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| "Could not read the device.")?;
        if bytes.len() as u64 > limit {
            return Err("The device returned too much data.");
        }
        let status = child
            .wait()
            .await
            .map_err(|_| "Could not finish the device check.")?;
        if !status.success() {
            return Err("Could not access the device. Check its permissions and availability.");
        }
        Ok(bytes)
    })
    .await
    .map_err(|_| "The device check timed out.".to_owned())?
    .map_err(str::to_owned)
}

/// A 16 kHz mono signed little-endian PCM microphone pipe, stopped when dropped.
pub fn audio_input(selection: &Selection) -> Result<Child, String> {
    super::validate_selection(selection, false)?;
    let mut cmd = microphone_command(&selection.microphone);
    cmd.args(["-ac", "1", "-ar", "16000", "-f", "s16le", "pipe:1"]);
    spawn(&mut cmd, false)
}

/// A PCM speaker pipe matching [`audio_input`], stopped when dropped.
pub fn audio_output(selection: &Selection) -> Result<Child, String> {
    super::validate_selection(selection, false)?;
    let mut cmd = ffmpeg();
    cmd.args(["-f", "s16le", "-ar", "16000", "-ac", "1", "-i", "pipe:0"]);
    speaker_args(&mut cmd, &selection.speaker);
    spawn(&mut cmd, true)
}

/// A paced 15 fps Annex-B H.264 camera pipe, with access-unit delimiters.
pub fn camera_stream(selection: &Selection) -> Result<Child, String> {
    let mut cmd = camera_command(&selection.camera)?;
    cmd.args([
        "-an",
        "-vf",
        "scale=640:480:force_original_aspect_ratio=decrease",
        "-r",
        "15",
        "-c:v",
        "libx264",
        "-threads",
        "2",
        "-preset",
        "ultrafast",
        "-tune",
        "zerolatency",
        "-x264-params",
        "aud=1:repeat-headers=1:keyint=30",
        "-f",
        "h264",
        "pipe:1",
    ]);
    spawn(&mut cmd, false)
}

/// Decode H.264 stdin to bounded-size PNG frames for the native call view.
pub fn decode_video() -> Result<Child, String> {
    let mut cmd = ffmpeg();
    cmd.args(["-max_pixels", "8294400"]);
    cmd.args([
        "-f",
        "h264",
        "-i",
        "pipe:0",
        "-vf",
        "fps=10,scale=640:480:force_original_aspect_ratio=decrease",
        "-f",
        "image2pipe",
        "-vcodec",
        "png",
        "-threads",
        "2",
        "pipe:1",
    ]);
    spawn(&mut cmd, true)
}

/// Read one framed PNG, refusing oversized chunks before allocating them.
pub async fn read_png(reader: &mut (impl AsyncRead + Unpin)) -> Result<Vec<u8>, String> {
    let mut signature = [0; 8];
    reader
        .read_exact(&mut signature)
        .await
        .map_err(|_| "Video stream ended")?;
    if signature != *b"\x89PNG\r\n\x1a\n" {
        return Err("Invalid video frame".into());
    }
    let mut png = signature.to_vec();
    loop {
        let mut header = [0; 8];
        reader
            .read_exact(&mut header)
            .await
            .map_err(|_| "Video stream ended")?;
        let len = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
        if len > 4 * 1024 * 1024 || png.len() + len + 12 > 4 * 1024 * 1024 {
            return Err("Video frame exceeds the limit".into());
        }
        let at = png.len();
        png.extend_from_slice(&header);
        png.resize(at + len + 12, 0);
        reader
            .read_exact(&mut png[at + 8..])
            .await
            .map_err(|_| "Video stream ended")?;
        if &header[4..] == b"IEND" {
            return Ok(png);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn png_reader_bounds_chunks_and_leaves_the_next_frame_intact() {
        let mut excessive = b"\x89PNG\r\n\x1a\n".to_vec();
        excessive.extend_from_slice(&(4u32 * 1024 * 1024 + 1).to_be_bytes());
        excessive.extend_from_slice(b"IDAT");
        assert!(read_png(&mut excessive.as_slice()).await.is_err());
        let frame = b"\x89PNG\r\n\x1a\n\0\0\0\0IEND\0\0\0\0";
        let joined = [frame.as_slice(), frame.as_slice()].concat();
        let mut reader = joined.as_slice();
        assert_eq!(read_png(&mut reader).await.unwrap(), frame);
        assert_eq!(read_png(&mut reader).await.unwrap(), frame);
        assert!(read_png(&mut frame[..12].as_ref()).await.is_err());
    }
}
