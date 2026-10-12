//! Locating and running ffmpeg / ffprobe.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaInfo {
    pub duration: f64,
    pub has_video: bool,
    pub has_audio: bool,
}

/// Resolve a tool: `$STRIKECUT_FFMPEG`-style override, then next to our own executable (release builds bundle
/// `strikecut-ffmpeg`, prefixed so it can't clash with a system ffmpeg in `/usr/bin`), then `PATH`.
fn tool(name: &str) -> PathBuf {
    let env_key = format!("STRIKECUT_{}", name.to_uppercase());
    if let Some(p) = std::env::var_os(env_key) {
        return PathBuf::from(p);
    }
    let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    if let Some(dir) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)) {
        for candidate in [format!("strikecut-{file}"), file.clone()] {
            let sibling = dir.join(candidate);
            if sibling.is_file() {
                return sibling;
            }
        }
    }
    PathBuf::from(file)
}

pub fn ffmpeg() -> PathBuf {
    tool("ffmpeg")
}

pub fn ffprobe() -> PathBuf {
    tool("ffprobe")
}

/// A `Command` that doesn't flash a console window when launched from the desktop app on Windows.
pub fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Run a command, turning a non-zero exit into an error that includes the tail of stderr.
pub fn run(cmd: &mut Command) -> Result<()> {
    let program = cmd.get_program().to_string_lossy().into_owned();
    let out = cmd.output().with_context(|| format!("failed to run `{program}`; is it installed and on PATH?"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> = stderr.lines().rev().take(15).collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
        bail!("`{program}` exited with {}:\n{}", out.status, tail.join("\n"));
    }
    Ok(())
}

pub fn probe(path: &Path) -> Result<MediaInfo> {
    #[derive(Deserialize)]
    struct Probe {
        #[serde(default)]
        streams: Vec<Stream>,
        format: Format,
    }
    #[derive(Deserialize)]
    struct Stream {
        codec_type: String,
        #[serde(default)]
        disposition: Disposition,
    }
    #[derive(Deserialize, Default)]
    struct Disposition {
        #[serde(default)]
        attached_pic: u8,
    }
    #[derive(Deserialize)]
    struct Format {
        duration: Option<String>,
    }

    let out = command(ffprobe())
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration:stream=codec_type:stream_disposition=attached_pic",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .context("failed to run ffprobe; is ffmpeg installed and on PATH?")?;
    if !out.status.success() {
        bail!("ffprobe could not read {}: {}", path.display(), String::from_utf8_lossy(&out.stderr).trim());
    }
    let p: Probe = serde_json::from_slice(&out.stdout).context("unexpected ffprobe output")?;
    let duration = p.format.duration.and_then(|d| d.parse::<f64>().ok()).context("media has no duration")?;
    Ok(MediaInfo {
        duration,
        // Cover art in audio files shows up as a video stream; ignore it.
        has_video: p.streams.iter().any(|s| s.codec_type == "video" && s.disposition.attached_pic == 0),
        has_audio: p.streams.iter().any(|s| s.codec_type == "audio"),
    })
}

/// Read the samples of a 16-bit PCM mono WAV, as written by [`extract_speech_wav`].
pub fn read_pcm16_wav(path: &Path) -> Result<Vec<i16>> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        bail!("{} is not a WAV file", path.display());
    }
    // Walk the RIFF chunks to "data"; ffmpeg may emit a LIST chunk before it.
    let mut pos = 12;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let len = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let body = pos + 8;
        if id == b"data" {
            let end = body.saturating_add(len).min(bytes.len());
            return Ok(bytes[body..end].as_chunks::<2>().0.iter().map(|&b| i16::from_le_bytes(b)).collect());
        }
        pos = body + len + (len & 1);
    }
    bail!("{} has no audio data", path.display())
}

/// Decode the first audio track to 16 kHz mono 16-bit PCM WAV, the input format whisper expects.
pub fn extract_speech_wav(input: &Path, wav_out: &Path) -> Result<()> {
    run(command(ffmpeg())
        .args(["-y", "-v", "error", "-i"])
        .arg(input)
        .args(["-vn", "-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le"])
        .arg(wav_out))
}
