//! The whisper speech models Strikecut knows about, and downloading them.
//!
//! Models come from whisper.cpp's Hugging Face repository. Every download is checked against the size and SHA-256
//! published there, written to a `.part` file, and only renamed into place once verified.

use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

/// Where models are downloaded from. Override with `$STRIKECUT_MODEL_MIRROR` (a URL that serves the same file names).
pub const DEFAULT_MIRROR: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Model {
    /// Short name used on the command line, e.g. `base.en`.
    pub name: &'static str,
    pub file: &'static str,
    pub size: u64,
    pub sha256: &'static str,
    pub about: &'static str,
}

/// The default model: English, fast on any CPU, accurate enough for clear speech.
pub const DEFAULT: &Model = &MODELS[1];

pub const MODELS: &[Model] = &[
    Model {
        name: "tiny.en",
        file: "ggml-tiny.en.bin",
        size: 77_704_715,
        sha256: "921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f",
        about: "English, fastest, least accurate",
    },
    Model {
        name: "base.en",
        file: "ggml-base.en.bin",
        size: 147_964_211,
        sha256: "a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002",
        about: "English, fast (default)",
    },
    Model {
        name: "small.en",
        file: "ggml-small.en.bin",
        size: 487_614_201,
        sha256: "c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d",
        about: "English, more accurate, ~3x slower",
    },
    Model {
        name: "medium.en",
        file: "ggml-medium.en.bin",
        size: 1_533_774_781,
        sha256: "cc37e93478338ec7700281a7ac30a10128929eb8f427dda2e865faa8f6da4356",
        about: "English, most accurate, slow without a GPU",
    },
    Model {
        name: "base",
        file: "ggml-base.bin",
        size: 147_951_465,
        sha256: "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
        about: "99 languages, fast",
    },
    Model {
        name: "small",
        file: "ggml-small.bin",
        size: 487_601_967,
        sha256: "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b",
        about: "99 languages, more accurate",
    },
    Model {
        name: "large-v3-turbo-q5_0",
        file: "ggml-large-v3-turbo-q5_0.bin",
        size: 574_041_195,
        sha256: "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
        about: "99 languages, best accuracy, slow without a GPU",
    },
];

/// Look a model up by short name (`small.en`) or file name (`ggml-small.en.bin`).
pub fn find(name: &str) -> Option<&'static Model> {
    MODELS.iter().find(|m| m.name == name || m.file == name)
}

/// `<data dir>/strikecut/models`: `%LOCALAPPDATA%` on Windows, `~/Library/Application Support` on macOS,
/// `$XDG_DATA_HOME` or `~/.local/share` elsewhere.
pub fn models_dir() -> PathBuf {
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    let data = if cfg!(windows) {
        env("LOCALAPPDATA")
    } else if cfg!(target_os = "macos") {
        env("HOME").map(|h| h.join("Library/Application Support"))
    } else {
        env("XDG_DATA_HOME").or_else(|| env("HOME").map(|h| h.join(".local/share")))
    };
    data.unwrap_or_else(|| PathBuf::from(".")).join("strikecut").join("models")
}

impl Model {
    pub fn path(&self) -> PathBuf {
        models_dir().join(self.file)
    }

    pub fn is_installed(&self) -> bool {
        std::fs::metadata(self.path()).is_ok_and(|m| m.len() == self.size)
    }

    pub fn url(&self) -> String {
        let mirror = std::env::var("STRIKECUT_MODEL_MIRROR").unwrap_or_else(|_| DEFAULT_MIRROR.to_string());
        format!("{}/{}", mirror.trim_end_matches('/'), self.file)
    }

    /// Download into [`models_dir`] unless already there. `progress(bytes_done, bytes_total)` is called as data arrives.
    pub fn download(&self, progress: impl FnMut(u64, u64)) -> Result<PathBuf> {
        let dest = self.path();
        if self.is_installed() {
            return Ok(dest);
        }
        self.download_to(&dest, progress)?;
        Ok(dest)
    }

    pub fn download_to(&self, dest: &Path, mut progress: impl FnMut(u64, u64)) -> Result<()> {
        let url = self.url();
        if let Some(dir) = dest.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let part = dest.with_extension("bin.part");
        let result = (|| -> Result<()> {
            let response = ureq::get(&url).call().with_context(|| format!("downloading {url}"))?;
            let mut body = response.into_body().into_reader();
            let mut out = BufWriter::new(File::create(&part).with_context(|| format!("creating {}", part.display()))?);
            let mut hasher = Sha256::new();
            let mut buf = vec![0u8; 256 * 1024];
            let mut done = 0u64;
            let mut last_report = 0u64;
            progress(0, self.size);
            loop {
                let n = body.read(&mut buf).with_context(|| format!("downloading {url}"))?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
                out.write_all(&buf[..n])?;
                done += n as u64;
                if done > self.size {
                    bail!("{url} is larger than the expected {} bytes", self.size);
                }
                if done - last_report >= 1 << 20 || done == self.size {
                    progress(done, self.size);
                    last_report = done;
                }
            }
            out.flush()?;
            drop(out);
            if done != self.size {
                bail!("download of {url} stopped early ({done} of {} bytes)", self.size);
            }
            let digest = hex(&hasher.finalize());
            if digest != self.sha256 {
                bail!("{} is corrupt: SHA-256 {digest}, expected {}", self.file, self.sha256);
            }
            std::fs::rename(&part, dest).with_context(|| format!("moving model into {}", dest.display()))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&part);
        }
        result
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_models_by_name_or_file() {
        assert_eq!(find("base.en"), Some(DEFAULT));
        assert_eq!(find("ggml-small.bin").map(|m| m.name), Some("small"));
        assert_eq!(find("huge"), None);
        assert_eq!(DEFAULT.file, crate::transcribe::DEFAULT_MODEL);
    }

    #[test]
    fn catalog_is_well_formed() {
        for m in MODELS {
            assert_eq!(m.file, format!("ggml-{}.bin", m.name));
            assert_eq!(m.sha256.len(), 64);
            assert!(m.sha256.bytes().all(|b| b.is_ascii_hexdigit()));
        }
    }

    #[test]
    fn hex_encodes() {
        assert_eq!(hex(&[0x00, 0xab, 0x10]), "00ab10");
    }
}
