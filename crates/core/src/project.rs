//! The `.strikecut.json` project file: media path, transcript with edits, and cut settings.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::edit::{keep_spans, CutOptions, Span};
use crate::transcript::{normalize, Transcript};

pub const PROJECT_VERSION: u32 = 1;
pub const PROJECT_SUFFIX: &str = ".strikecut.json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub version: u32,
    pub media: PathBuf,
    pub duration: f64,
    #[serde(default)]
    pub has_video: bool,
    #[serde(default)]
    pub cut: CutOptions,
    pub transcript: Transcript,
}

impl Project {
    pub fn new(media: PathBuf, duration: f64, has_video: bool, transcript: Transcript) -> Self {
        Self { version: PROJECT_VERSION, media, duration, has_video, cut: CutOptions::default(), transcript }
    }

    /// Default project location for a media file: `talk.mp4` -> `talk.mp4.strikecut.json`.
    pub fn default_path(media: &Path) -> PathBuf {
        let mut name = media.file_name().unwrap_or_default().to_os_string();
        name.push(PROJECT_SUFFIX);
        media.with_file_name(name)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let mut p: Project = serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        if p.version > PROJECT_VERSION {
            bail!("{} was made by a newer Strikecut (project v{})", path.display(), p.version);
        }
        // Media paths are stored relative to the project file when possible.
        if p.media.is_relative() {
            if let Some(dir) = path.parent() {
                p.media = dir.join(&p.media);
            }
        }
        Ok(p)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let mut out = self.clone();
        let dir = std::path::absolute(path)?.parent().map(Path::to_path_buf);
        let media = std::path::absolute(&self.media)?;
        if let Some(rel) = dir.and_then(|dir| media.strip_prefix(dir).ok().map(Path::to_path_buf)) {
            out.media = rel.to_path_buf();
        }
        let json = serde_json::to_string_pretty(&out)?;
        // Write-then-rename so a crash never leaves a half-written project.
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, json).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    pub fn spans(&self) -> Vec<Span> {
        keep_spans(&self.transcript.words, self.duration, &self.cut)
    }

    /// Set the deleted flag on words `from..=to` (indices). Returns how many changed.
    pub fn set_deleted(&mut self, from: usize, to: usize, deleted: bool) -> Result<usize> {
        let n = self.transcript.words.len();
        if from > to || to >= n {
            bail!("word range {from}-{to} is out of bounds (transcript has {n} words)");
        }
        let mut changed = 0;
        for w in &mut self.transcript.words[from..=to] {
            changed += usize::from(w.deleted != deleted);
            w.deleted = deleted;
        }
        Ok(changed)
    }

    /// Delete every occurrence of a phrase (case- and punctuation-insensitive). Returns the number of occurrences.
    pub fn delete_phrase(&mut self, phrase: &str) -> usize {
        let needle: Vec<String> = phrase.split_whitespace().map(normalize).filter(|s| !s.is_empty()).collect();
        if needle.is_empty() {
            return 0;
        }
        let hay: Vec<String> = self.transcript.words.iter().map(|w| w.normalized()).collect();
        let mut hits = 0;
        let mut i = 0;
        while i + needle.len() <= hay.len() {
            if hay[i..i + needle.len()] == needle[..] {
                for w in &mut self.transcript.words[i..i + needle.len()] {
                    w.deleted = true;
                }
                hits += 1;
                i += needle.len();
            } else {
                i += 1;
            }
        }
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::Word;

    fn project(text: &str) -> Project {
        let words = text.split_whitespace().enumerate().map(|(i, s)| Word::new(s, i as f64, i as f64 + 0.8)).collect();
        Project::new("talk.mp4".into(), 10.0, true, Transcript::new(words))
    }

    #[test]
    fn deletes_phrases() {
        let mut p = project("So, you know, it works. You know?");
        assert_eq!(p.delete_phrase("you know"), 2);
        assert_eq!(p.transcript.kept_text(), "So, it works.");
    }

    #[test]
    fn range_bounds_are_checked() {
        let mut p = project("a b c");
        assert_eq!(p.set_deleted(1, 2, true).unwrap(), 2);
        assert!(p.set_deleted(2, 3, true).is_err());
    }

    #[test]
    fn save_load_roundtrip_with_relative_media() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = project("hello world");
        p.media = dir.path().join("talk.mp4");
        let path = Project::default_path(&p.media);
        p.save(&path).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("\"media\": \"talk.mp4\""));
        assert_eq!(Project::load(&path).unwrap(), p);
    }
}
