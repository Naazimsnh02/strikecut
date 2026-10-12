use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use strikecut_core::edit::output_duration;
use strikecut_core::export::{export, ExportOptions};
use strikecut_core::filler::{mark_fillers, DEFAULT_FILLERS};
use strikecut_core::project::PROJECT_SUFFIX;
use strikecut_core::srt::{to_srt, CaptionOptions};
use strikecut_core::transcribe::{align_with_media, default_model_path, import_json, transcribe, TranscribeOptions};
use strikecut_core::{media, Project};

/// Edit video by editing text. Delete words from the transcript and they are cut from the video.
#[derive(Parser)]
#[command(version, about, propagate_version = true)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// One shot: transcribe, remove filler words (and optionally long pauses), export.
    Clean {
        media: PathBuf,
        /// Output file [default: <name>.clean.<ext>]
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Shorten pauses longer than this many seconds.
        #[arg(long, value_name = "SECONDS")]
        max_pause: Option<f64>,
        /// Don't remove filler words.
        #[arg(long)]
        keep_fillers: bool,
        /// Also write captions next to the output.
        #[arg(long)]
        srt: bool,
        #[command(flatten)]
        whisper: WhisperArgs,
    },
    /// Transcribe media into a project file (<media>.strikecut.json).
    Transcribe {
        media: PathBuf,
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Overwrite an existing project (this discards edits).
        #[arg(long)]
        force: bool,
        #[command(flatten)]
        whisper: WhisperArgs,
    },
    /// Create a project from a transcript made by another tool (whisper.cpp, WhisperX, faster-whisper, OpenAI JSON).
    Import {
        media: PathBuf,
        transcript: PathBuf,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Print the transcript. Deleted words are shown as ~~strikethrough~~.
    Text {
        project: PathBuf,
        /// Prefix every word with its index (for `delete --range`).
        #[arg(long, conflicts_with = "json")]
        indices: bool,
        /// Print words as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Mark words as deleted.
    Delete {
        project: PathBuf,
        #[command(flatten)]
        target: Target,
    },
    /// Un-delete words.
    Restore {
        project: PathBuf,
        /// Word indices, e.g. `12-30` or `7`.
        #[arg(long, value_parser = parse_range, conflicts_with = "all")]
        range: Option<(usize, usize)>,
        #[arg(long)]
        all: bool,
    },
    /// Change how cuts are made.
    Set {
        project: PathBuf,
        /// Shorten pauses longer than this many seconds.
        #[arg(long, value_name = "SECONDS", conflicts_with = "keep_pauses")]
        max_pause: Option<f64>,
        /// Keep natural pauses (undo --max-pause).
        #[arg(long)]
        keep_pauses: bool,
        /// Seconds of lead-in kept before a word that follows a cut.
        #[arg(long, value_name = "SECONDS")]
        padding: Option<f64>,
    },
    /// Render the edited media.
    Export {
        project: PathBuf,
        /// Output file [default: <name>.edited.<ext>]
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Also write captions next to the output.
        #[arg(long)]
        srt: bool,
        /// Video quality (x264 CRF, lower is better).
        #[arg(long, default_value_t = 20)]
        crf: u8,
    },
}

#[derive(Args)]
struct WhisperArgs {
    /// ggml whisper model [default: $STRIKECUT_MODEL or the app data dir]
    #[arg(long)]
    model: Option<PathBuf>,
    /// Spoken language (ISO code) or `auto`.
    #[arg(long, default_value = "auto")]
    language: String,
    #[arg(long)]
    threads: Option<u16>,
}

impl WhisperArgs {
    fn options(&self) -> TranscribeOptions {
        TranscribeOptions {
            model: self.model.clone().unwrap_or_else(default_model_path),
            language: Some(self.language.clone()),
            threads: self.threads,
        }
    }
}

#[derive(Args)]
#[group(required = true, multiple = false)]
struct Target {
    /// Word indices, e.g. `12-30` or `7` (see `strikecut text --indices`).
    #[arg(long, value_parser = parse_range)]
    range: Option<(usize, usize)>,
    /// Every occurrence of a phrase, e.g. "you know".
    #[arg(long)]
    phrase: Option<String>,
    /// Filler words (um, uh, ...).
    #[arg(long)]
    fillers: bool,
}

fn parse_range(s: &str) -> Result<(usize, usize), String> {
    let parse = |x: &str| x.trim().parse::<usize>().map_err(|_| format!("`{s}` is not a word index or range like 12-30"));
    match s.split_once(['-', ':']) {
        Some((a, b)) => Ok((parse(a)?, parse(b)?)),
        None => parse(s).map(|i| (i, i)),
    }
}

fn main() {
    if let Err(e) = run(Cli::parse()) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Cmd::Clean { media, output, max_pause, keep_fillers, srt, whisper } => {
            let project_path = Project::default_path(&media);
            let mut project = if project_path.is_file() {
                eprintln!("Reusing transcript from {}", project_path.display());
                Project::load(&project_path)?
            } else {
                new_project(&media, &whisper)?
            };
            if !keep_fillers {
                let n = mark_fillers(&mut project.transcript, DEFAULT_FILLERS);
                eprintln!("Removed {n} filler words");
            }
            if max_pause.is_some() {
                project.cut.max_pause = max_pause;
            }
            project.save(&project_path)?;
            let output = output.unwrap_or_else(|| sibling(&media, "clean"));
            render(&project, &output, srt, &ExportOptions::default())
        }
        Cmd::Transcribe { media, output, force, whisper } => {
            let path = output.unwrap_or_else(|| Project::default_path(&media));
            if path.exists() && !force {
                bail!("{} already exists (use --force to re-transcribe and discard its edits)", path.display());
            }
            let project = new_project(&media, &whisper)?;
            project.save(&path)?;
            println!("{}", path.display());
            Ok(())
        }
        Cmd::Import { media, transcript, output } => {
            let info = media::probe(&media)?;
            let mut t = import_json(&transcript)?;
            align_with_media(&media, &mut t)?;
            let words = t.words.len();
            let project = Project::new(absolute(&media)?, info.duration, info.has_video, t);
            let path = output.unwrap_or_else(|| Project::default_path(&media));
            project.save(&path)?;
            eprintln!("Imported {words} words");
            println!("{}", path.display());
            Ok(())
        }
        Cmd::Text { project, indices, json } => {
            let p = Project::load(&project_path(&project))?;
            if json {
                let words: Vec<_> = p
                    .transcript
                    .words
                    .iter()
                    .enumerate()
                    .map(|(i, w)| serde_json::json!({"i": i, "text": w.text, "start": w.start, "end": w.end, "deleted": w.deleted}))
                    .collect();
                println!("{}", serde_json::to_string_pretty(&words)?);
            } else if indices {
                for (i, w) in p.transcript.words.iter().enumerate() {
                    let mark = if w.deleted { "~~" } else { "" };
                    print!("[{i}]{mark}{}{mark} ", w.text.trim());
                }
                println!();
            } else {
                println!("{}", p.transcript.marked_text());
            }
            Ok(())
        }
        Cmd::Delete { project, target } => edit(&project, |p| {
            Ok(match target {
                Target { range: Some((a, b)), .. } => format!("Deleted {} words", p.set_deleted(a, b, true)?),
                Target { phrase: Some(ph), .. } => format!("Deleted {} occurrence(s) of \"{ph}\"", p.delete_phrase(&ph)),
                _ => format!("Deleted {} filler words", mark_fillers(&mut p.transcript, DEFAULT_FILLERS)),
            })
        }),
        Cmd::Restore { project, range, all } => edit(&project, |p| {
            let (a, b) = match (range, all) {
                (Some(r), _) => r,
                (None, true) => (0, p.transcript.words.len().saturating_sub(1)),
                (None, false) => bail!("pass --range or --all"),
            };
            Ok(format!("Restored {} words", p.set_deleted(a, b, false)?))
        }),
        Cmd::Set { project, max_pause, keep_pauses, padding } => edit(&project, |p| {
            if keep_pauses {
                p.cut.max_pause = None;
            } else if max_pause.is_some() {
                p.cut.max_pause = max_pause;
            }
            if let Some(pad) = padding {
                p.cut.padding = pad;
            }
            Ok(serde_json::to_string(&p.cut)?)
        }),
        Cmd::Export { project, output, srt, crf } => {
            let p = Project::load(&project_path(&project))?;
            let output = output.unwrap_or_else(|| sibling(&p.media, "edited"));
            render(&p, &output, srt, &ExportOptions { crf, ..Default::default() })
        }
    }
}

fn new_project(media_path: &Path, whisper: &WhisperArgs) -> Result<Project> {
    let info = media::probe(media_path)?;
    eprintln!("Transcribing {} ({:.0}s of media)...", media_path.display(), info.duration);
    let t = transcribe(media_path, &whisper.options())?;
    eprintln!("Transcribed {} words", t.words.len());
    Ok(Project::new(absolute(media_path)?, info.duration, info.has_video, t))
}

fn render(p: &Project, output: &Path, srt: bool, opts: &ExportOptions) -> Result<()> {
    let spans = p.spans();
    let before = p.duration;
    let after = output_duration(&spans);
    export(&p.media, &spans, output, opts, |f| {
        eprint!("\rExporting... {:>3.0}%", f * 100.0);
        let _ = std::io::stderr().flush();
    })?;
    eprintln!();
    if srt {
        let path = output.with_extension("srt");
        std::fs::write(&path, to_srt(&p.transcript.words, &spans, &CaptionOptions::default()))?;
        eprintln!("Captions: {}", path.display());
    }
    eprintln!("{} -> {} ({:.1}s saved)", fmt_time(before), fmt_time(after), before - after);
    println!("{}", output.display());
    Ok(())
}

/// Load a project, apply an edit, save it, and report what changed.
fn edit(path: &Path, f: impl FnOnce(&mut Project) -> Result<String>) -> Result<()> {
    let path = project_path(path);
    let mut p = Project::load(&path)?;
    let msg = f(&mut p)?;
    p.save(&path)?;
    eprintln!("{msg}");
    eprintln!("Edited length: {}", fmt_time(output_duration(&p.spans())));
    Ok(())
}

/// Accept either a project file or the media file it belongs to.
fn project_path(path: &Path) -> PathBuf {
    if path.to_string_lossy().ends_with(PROJECT_SUFFIX) {
        path.to_path_buf()
    } else {
        Project::default_path(path)
    }
}

/// `talk.mp4` + "clean" -> `talk.clean.mp4`
fn sibling(media: &Path, tag: &str) -> PathBuf {
    let stem = media.file_stem().unwrap_or_default().to_string_lossy();
    match media.extension() {
        Some(ext) => media.with_file_name(format!("{stem}.{tag}.{}", ext.to_string_lossy())),
        None => media.with_file_name(format!("{stem}.{tag}")),
    }
}

fn absolute(p: &Path) -> Result<PathBuf> {
    std::path::absolute(p).with_context(|| format!("resolving {}", p.display()))
}

fn fmt_time(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}
