// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};

use serde::Serialize;
use strikecut_core::edit::output_duration;
use strikecut_core::export::{export, ExportOptions};
use strikecut_core::filler::{mark_fillers, DEFAULT_FILLERS};
use strikecut_core::srt::{to_srt, CaptionOptions};
use strikecut_core::transcribe::{align_with_media, default_model_path, import_json, transcribe, TranscribeOptions};
use strikecut_core::{media, models, Project, Span};
use tauri::{AppHandle, Emitter, Manager};

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    format!("{e:#}")
}

/// Run blocking work (ffmpeg, whisper) off the UI thread.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> CmdResult<T> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(err)?
        .map_err(|e| format!("{e:#}"))
}

/// Let the webview stream this file through the asset protocol. Nothing else on disk is exposed.
fn allow_media(app: &AppHandle, path: &Path) -> CmdResult<()> {
    app.asset_protocol_scope().allow_file(path).map_err(err)
}

#[derive(Serialize)]
struct ModelStatus {
    path: PathBuf,
    exists: bool,
    /// Download size in bytes if the default model can be fetched automatically.
    download_size: Option<u64>,
}

#[tauri::command]
fn model_status() -> ModelStatus {
    let path = default_model_path();
    let downloadable = (path == models::DEFAULT.path()).then_some(models::DEFAULT.size);
    ModelStatus { exists: path.is_file(), path, download_size: downloadable }
}

/// Fetch the default speech model, emitting `model-progress` as `[done, total]` bytes.
#[tauri::command]
async fn download_model(app: AppHandle) -> CmdResult<PathBuf> {
    blocking(move || {
        models::DEFAULT.download(|done, total| {
            let _ = app.emit("model-progress", (done, total));
        })
    })
    .await
}

/// A media file passed on the command line (`strikecut-app talk.mp4`, or "Open with" from the file manager).
#[tauri::command]
fn startup_file() -> Option<PathBuf> {
    std::env::args_os().skip(1).map(PathBuf::from).find(|p| p.is_file())
}

/// Open media. Returns the existing project if this file was edited before, otherwise `None` (needs a transcript).
#[tauri::command]
fn open_media(app: AppHandle, media: PathBuf) -> CmdResult<Option<Project>> {
    allow_media(&app, &media)?;
    let path = Project::default_path(&media);
    if path.is_file() {
        let p = Project::load(&path).map_err(err)?;
        allow_media(&app, &p.media)?;
        return Ok(Some(p));
    }
    Ok(None)
}

#[tauri::command]
async fn transcribe_media(media: PathBuf, model: Option<PathBuf>, language: Option<String>) -> CmdResult<Project> {
    blocking(move || {
        let info = media::probe(&media)?;
        let opts = TranscribeOptions {
            model: model.unwrap_or_else(default_model_path),
            language,
            threads: None,
        };
        let t = transcribe(&media, &opts)?;
        let p = Project::new(media.clone(), info.duration, info.has_video, t);
        p.save(&Project::default_path(&media))?;
        Ok(p)
    })
    .await
}

#[tauri::command]
async fn import_transcript(media: PathBuf, transcript: PathBuf) -> CmdResult<Project> {
    blocking(move || {
        let info = media::probe(&media)?;
        let mut t = import_json(&transcript)?;
        align_with_media(&media, &mut t)?;
        let p = Project::new(media.clone(), info.duration, info.has_video, t);
        p.save(&Project::default_path(&media))?;
        Ok(p)
    })
    .await
}

#[tauri::command]
fn save_project(project: Project) -> CmdResult<()> {
    project.save(&Project::default_path(&project.media)).map_err(err)
}

#[derive(Serialize)]
struct Cut {
    spans: Vec<Span>,
    duration: f64,
}

#[tauri::command]
fn keep_spans(project: Project) -> Cut {
    let spans = project.spans();
    Cut { duration: output_duration(&spans), spans }
}

#[tauri::command]
fn remove_fillers(mut project: Project) -> (Project, usize) {
    let n = mark_fillers(&mut project.transcript, DEFAULT_FILLERS);
    (project, n)
}

#[tauri::command]
async fn export_project(app: AppHandle, project: Project, output: PathBuf, srt: bool) -> CmdResult<PathBuf> {
    blocking(move || {
        let spans = project.spans();
        export(&project.media, &spans, &output, &ExportOptions::default(), |f| {
            let _ = app.emit("export-progress", f);
        })?;
        if srt {
            std::fs::write(
                output.with_extension("srt"),
                to_srt(&project.transcript.words, &spans, &CaptionOptions::default()),
            )?;
        }
        Ok(output)
    })
    .await
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            model_status,
            download_model,
            startup_file,
            open_media,
            transcribe_media,
            import_transcript,
            save_project,
            keep_spans,
            remove_fillers,
            export_project
        ])
        .run(tauri::generate_context!())
        .expect("error while running Strikecut");
}
