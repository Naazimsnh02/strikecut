//! Speech-to-text with word-level timestamps.
//!
//! Backends, in order of preference:
//! 1. Embedded whisper.cpp (`--features whisper`), fully self-contained.
//! 2. An external `whisper-cli` from whisper.cpp (`$STRIKECUT_WHISPER_CLI` or on `PATH`).
//!
//! Transcripts from other tools can be brought in with [`import_json`].

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::transcript::{Transcript, Word};
use crate::{align, media, models};

#[derive(Debug, Clone)]
pub struct TranscribeOptions {
    /// Path to a ggml whisper model, e.g. `ggml-base.en.bin`.
    pub model: PathBuf,
    /// ISO language code, or `None` / `"auto"` to detect.
    pub language: Option<String>,
    pub threads: Option<u16>,
}

pub const DEFAULT_MODEL: &str = "ggml-base.en.bin";

/// Whisper is trained on cleaned-up captions, so it quietly drops "um" and "uh". Starting it off with text full of
/// fillers makes it transcribe them verbatim, which is what filler removal needs.
pub const VERBATIM_PROMPT: &str = "Umm, so, uh, I was like, hmm... Okay, um, here's what I, uh, mean.";

/// The verbatim prompt is English, so only use it for English: an English-only model or `--language en`.
fn verbatim_prompt(opts: &TranscribeOptions) -> Option<&'static str> {
    let english_model = opts.model.file_name().is_some_and(|n| n.to_string_lossy().contains(".en"));
    let english = english_model || opts.language.as_deref() == Some("en");
    english.then_some(VERBATIM_PROMPT)
}

/// The model used when none is given: `$STRIKECUT_MODEL`, else `ggml-base.en.bin` in [`models::models_dir`].
pub fn default_model_path() -> PathBuf {
    match std::env::var_os("STRIKECUT_MODEL") {
        Some(p) => PathBuf::from(p),
        None => models::DEFAULT.path(),
    }
}

pub fn transcribe(input: &Path, opts: &TranscribeOptions) -> Result<Transcript> {
    if !opts.model.is_file() {
        bail!(
            "whisper model not found at {}\n\
             Download one (e.g. ggml-base.en.bin) from https://huggingface.co/ggerganov/whisper.cpp/tree/main \
             and pass --model or set STRIKECUT_MODEL",
            opts.model.display()
        );
    }
    let dir = tempfile::tempdir()?;
    let wav = dir.path().join("speech.wav");
    media::extract_speech_wav(input, &wav)?;

    let mut transcript = backend(&wav, dir.path(), opts)?;
    transcript.sanitize();
    align_words(&mut transcript, &media::read_pcm16_wav(&wav)?);
    Ok(transcript)
}

/// Tighten an existing transcript's word boundaries against the media's audio (e.g. after [`import_json`]).
pub fn align_with_media(input: &Path, transcript: &mut Transcript) -> Result<()> {
    let dir = tempfile::tempdir()?;
    let wav = dir.path().join("speech.wav");
    media::extract_speech_wav(input, &wav)?;
    align_words(transcript, &media::read_pcm16_wav(&wav)?);
    Ok(())
}

fn align_words(transcript: &mut Transcript, samples_16k: &[i16]) {
    let opts = align::AlignOptions::default();
    let db = align::frame_db(samples_16k, 16_000);
    let silences = align::silences(&db, opts.min_silence);
    let snapped = align::snap_to_silences(&mut transcript.words, &silences, opts.max_shift);
    align::snap_to_dips(&mut transcript.words, &snapped, &db, opts.dip_window);
}

#[cfg(feature = "whisper")]
fn backend(wav: &Path, _tmp: &Path, opts: &TranscribeOptions) -> Result<Transcript> {
    embedded::transcribe_wav(wav, opts)
}

#[cfg(not(feature = "whisper"))]
fn backend(wav: &Path, tmp: &Path, opts: &TranscribeOptions) -> Result<Transcript> {
    let cli = std::env::var_os("STRIKECUT_WHISPER_CLI")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("whisper-cli{}", std::env::consts::EXE_SUFFIX)));
    let base = tmp.join("out");
    let mut cmd = media::command(&cli);
    cmd.arg("-m").arg(&opts.model).arg("-f").arg(wav);
    // One word per segment, split on word boundaries, JSON output.
    cmd.args(["-ml", "1", "-sow", "-oj", "-np", "-of"]).arg(&base);
    cmd.arg("-l").arg(opts.language.as_deref().unwrap_or("auto"));
    if let Some(prompt) = verbatim_prompt(opts) {
        cmd.arg("--prompt").arg(prompt);
    }
    if let Some(t) = opts.threads {
        cmd.arg("-t").arg(t.to_string());
    }
    media::run(&mut cmd).with_context(|| {
        format!(
            "this build has no embedded whisper; tried `{}`. Install whisper.cpp's whisper-cli or build with --features whisper",
            cli.display()
        )
    })?;
    import_json(&base.with_extension("json"))
}

#[cfg(feature = "whisper")]
mod embedded {
    use super::*;
    use whisper_rs::{
        DtwMode, DtwModelPreset, DtwParameters, FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters,
    };

    /// DTW alignment needs the attention-head preset matching the model; infer it from the standard file names.
    fn dtw_preset(model: &Path) -> Option<DtwModelPreset> {
        let name = model.file_name()?.to_str()?.to_ascii_lowercase();
        let name = name.trim_start_matches("ggml-").split(['.', '-', '_']).collect::<Vec<_>>().join("-");
        use DtwModelPreset::*;
        Some(match name.as_str() {
            n if n.starts_with("tiny-en") => TinyEn,
            n if n.starts_with("tiny") => Tiny,
            n if n.starts_with("base-en") => BaseEn,
            n if n.starts_with("base") => Base,
            n if n.starts_with("small-en") => SmallEn,
            n if n.starts_with("small") => Small,
            n if n.starts_with("medium-en") => MediumEn,
            n if n.starts_with("medium") => Medium,
            n if n.starts_with("large-v3-turbo") => LargeV3Turbo,
            n if n.starts_with("large-v3") => LargeV3,
            n if n.starts_with("large-v2") => LargeV2,
            n if n.starts_with("large-v1") || n == "large-bin" => LargeV1,
            _ => return None,
        })
    }

    pub fn transcribe_wav(wav: &Path, opts: &TranscribeOptions) -> Result<Transcript> {
        // Route whisper.cpp/ggml's chatty stderr logging into the void.
        whisper_rs::install_logging_hooks();

        let samples = media::read_pcm16_wav(wav)?;
        let mut audio = vec![0.0f32; samples.len()];
        whisper_rs::convert_integer_to_float_audio(&samples, &mut audio)?;

        let mut cparams = WhisperContextParameters::default();
        let dtw = dtw_preset(&opts.model);
        if let Some(preset) = dtw.clone() {
            cparams.dtw_parameters(DtwParameters {
                mode: DtwMode::ModelPreset { model_preset: preset },
                ..Default::default()
            });
        }
        let ctx = WhisperContext::new_with_params(&opts.model, cparams).context("loading whisper model")?;
        let mut state = ctx.create_state()?;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_token_timestamps(true);
        params.set_split_on_word(true);
        params.set_max_len(1);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_special(false);
        params.set_print_timestamps(false);
        let lang = opts.language.as_deref().filter(|l| *l != "auto");
        params.set_language(Some(lang.unwrap_or("auto")));
        // whisper.cpp defaults to 4 threads; most machines have more cores than that.
        let threads = opts
            .threads
            .map(i32::from)
            .unwrap_or_else(|| std::thread::available_parallelism().map_or(4, |n| n.get().clamp(1, 8) as i32));
        params.set_n_threads(threads);
        if let Some(prompt) = verbatim_prompt(opts) {
            params.set_initial_prompt(prompt);
        }

        state.full(params, &audio)?;

        // With max_len = 1 each segment is one word. Timestamps are in centiseconds.
        let mut words = Vec::new();
        for i in 0..state.full_n_segments() {
            let Some(seg) = state.get_segment(i) else { continue };
            let text = seg.to_str_lossy()?.trim().to_string();
            if text.is_empty() {
                continue;
            }
            let mut start = seg.start_timestamp() as f64 / 100.0;
            let end = seg.end_timestamp() as f64 / 100.0;
            // DTW gives a much more accurate onset for the word's first text token.
            if dtw.is_some() {
                let onset = (0..seg.n_tokens())
                    .filter_map(|j| seg.get_token(j))
                    .find(|t| {
                        let s = t.to_str_lossy().unwrap_or_default();
                        !s.trim().is_empty() && !s.starts_with("[_") && !s.starts_with("<|")
                    })
                    .map(|t| t.token_data().t_dtw)
                    .filter(|&t| t >= 0);
                if let Some(t) = onset {
                    start = t as f64 / 100.0;
                }
            }
            if std::env::var_os("STRIKECUT_DEBUG_TIMING").is_some() {
                eprintln!("{text:>14} seg {:.2}-{:.2}  start {start:.2}", seg.start_timestamp() as f64 / 100.0, end);
            }
            words.push(Word::new(text, start, end.max(start)));
        }
        Ok(Transcript::new(words))
    }
}

/// Import a word-timestamped transcript produced by another tool. Supported shapes:
/// * whisper.cpp `-oj` / `-ojf` (`transcription[].offsets`, ideally run with `-ml 1 -sow`)
/// * WhisperX / faster-whisper / OpenAI verbose JSON (`segments[].words[]` or top-level `words[]`)
/// * Strikecut's own `{ "words": [{ "text", "start", "end" }] }`
pub fn import_json(path: &Path) -> Result<Transcript> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let v: Value = serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    let mut t =
        parse_transcript_json(&v).with_context(|| format!("unrecognized transcript format in {}", path.display()))?;
    t.sanitize();
    if t.words.is_empty() {
        bail!("{} contains no timestamped words", path.display());
    }
    Ok(t)
}

fn parse_transcript_json(v: &Value) -> Option<Transcript> {
    let language =
        v.pointer("/result/language").or_else(|| v.get("language")).and_then(Value::as_str).map(str::to_string);

    let words = if let Some(items) = v.get("transcription").and_then(Value::as_array) {
        whisper_cpp_words(items)
    } else if let Some(words) = v.get("words").and_then(Value::as_array) {
        words.iter().filter_map(generic_word).collect()
    } else {
        v.get("segments")
            .and_then(Value::as_array)?
            .iter()
            .filter_map(|s| s.get("words").and_then(Value::as_array))
            .flatten()
            .filter_map(generic_word)
            .collect()
    };
    Some(Transcript { language, words })
}

fn generic_word(w: &Value) -> Option<Word> {
    let text = w.get("text").or_else(|| w.get("word"))?.as_str()?.trim();
    // WhisperX omits timestamps for words it couldn't align (e.g. numerals); skip those.
    let start = w.get("start")?.as_f64()?;
    let end = w.get("end")?.as_f64()?;
    let mut word = Word::new(text, start, end);
    word.speaker = w
        .get("speaker")
        .and_then(|s| s.as_u64().map(|n| n as u32).or_else(|| s.as_str()?.trim_start_matches("SPEAKER_").parse().ok()));
    Some(word)
}

fn whisper_cpp_words(items: &[Value]) -> Vec<Word> {
    let mut words = Vec::new();
    for item in items {
        let Some(text) = item.get("text").and_then(Value::as_str) else { continue };
        let from = item.pointer("/offsets/from").and_then(Value::as_f64).unwrap_or(0.0) / 1000.0;
        let to = item.pointer("/offsets/to").and_then(Value::as_f64).unwrap_or(0.0) / 1000.0;
        // With `-ml 1` each item is one word. Otherwise spread the segment's words evenly over its duration.
        let parts: Vec<&str> = text.split_whitespace().collect();
        let step = (to - from) / parts.len().max(1) as f64;
        for (i, p) in parts.iter().enumerate() {
            if p.starts_with("[_") || p.starts_with("<|") {
                continue;
            }
            let s = from + step * i as f64;
            words.push(Word::new(*p, s, s + step));
        }
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_whisper_cpp() {
        let v = json!({
            "result": {"language": "en"},
            "transcription": [
                {"offsets": {"from": 0, "to": 400}, "text": " Hello"},
                {"offsets": {"from": 400, "to": 1000}, "text": " big world"}
            ]
        });
        let t = parse_transcript_json(&v).unwrap();
        assert_eq!(t.language.as_deref(), Some("en"));
        let got: Vec<_> = t.words.iter().map(|w| (w.text.as_str(), w.start, w.end)).collect();
        assert_eq!(got, [("Hello", 0.0, 0.4), ("big", 0.4, 0.7), ("world", 0.7, 1.0)]);
    }

    #[test]
    fn parses_whisperx_segments_and_skips_unaligned() {
        let v = json!({"segments": [{"words": [
            {"word": "Hi", "start": 0.1, "end": 0.3, "speaker": "SPEAKER_01"},
            {"word": "2026"},
            {"word": "there", "start": 0.4, "end": 0.6}
        ]}]});
        let t = parse_transcript_json(&v).unwrap();
        assert_eq!(t.words.len(), 2);
        assert_eq!(t.words[0].speaker, Some(1));
    }
}
