<div align="center">

# Strike~~cut~~

**Edit video by editing text.** Delete a word from the transcript and it's cut from the video.

Local. Open source. No account, no subscription, nothing uploaded.

<!-- TODO: hero GIF — select a sentence, press Delete, watch the video skip it -->

</div>

---

Strikecut is a free, open-source alternative to Descript's text-based editing, built in Rust as a single small app for Windows, macOS, and Linux. Speech is transcribed **on your machine** with [whisper.cpp](https://github.com/ggerganov/whisper.cpp), so your recordings never leave it.

## Features

- **Edit the transcript, cut the video.** Select words, press <kbd>Delete</kbd>. Press it again to restore.
- **Remove filler words in one click.** Um, uh, hmm... gone everywhere.
- **Shorten long pauses.** Cap every silence at 1.0s, 0.6s, or 0.3s.
- **Live preview.** Playback skips cut parts, so you hear the edit before exporting.
- **Frame-accurate export.** One streaming ffmpeg pass, with audio and video kept in sync across hundreds of cuts.
- **Captions for free.** Export `.srt` subtitles timed to the *edited* video.
- **A CLI built for scripts and AI agents.** Every edit is a command, and projects are plain JSON.

## The CLI

```sh
# One shot: transcribe, drop filler words, cap pauses at 0.5s, export with captions
strikecut clean talk.mp4 --max-pause 0.5 --srt

# Or step by step
strikecut transcribe talk.mp4                      # -> talk.mp4.strikecut.json
strikecut text talk.mp4 --indices                  # [0]So [1]um [2]today ...
strikecut delete talk.mp4 --range 120-164          # cut a tangent
strikecut delete talk.mp4 --phrase "you know"      # every occurrence
strikecut delete talk.mp4 --fillers
strikecut text talk.mp4                            # review: deleted words show as ~~this~~
strikecut export talk.mp4 -o final.mp4 --srt
```

Since edits are just commands over a readable transcript, you can hand a recording to a coding agent and say *"cut the part where I talk about pricing"* and it can do it.

Already have a transcript? `strikecut import talk.mp4 words.json` accepts whisper.cpp, WhisperX, faster-whisper, and OpenAI verbose JSON.

## Install

> Strikecut is pre-release. Binaries are coming. For now, build from source.

**Requirements:** [Rust](https://rustup.rs) 1.80+ and [ffmpeg](https://ffmpeg.org/download.html) 6+ on your `PATH`.

```sh
git clone https://github.com/strikecut/strikecut && cd strikecut

# CLI
cargo install --path crates/cli --features whisper

# Desktop app (also needs Node 20+ and pnpm)
cd app && pnpm install && pnpm tauri build -- --features whisper
```

`--features whisper` embeds whisper.cpp, which needs **CMake** and **Clang** at build time. Without it, Strikecut uses a `whisper-cli` binary from [whisper.cpp releases](https://github.com/ggerganov/whisper.cpp/releases) if one is on your `PATH`.

On Windows: `winget install Kitware.CMake LLVM.LLVM`, then set `LIBCLANG_PATH=C:\Program Files\LLVM\bin` before building.

**Speech model:** downloaded for you the first time you transcribe (148 MB, once; checked against its SHA-256). The app asks first; the CLI just does it. To pick a different model:

```sh
strikecut model list                   # tiny.en ... large-v3-turbo, and which are installed
strikecut model download small.en      # more accurate English
strikecut transcribe talk.mp4 --model small.en
strikecut transcribe talk.mp4 --model path/to/any-ggml-model.bin
```

Models live in `%LOCALAPPDATA%\strikecut\models` (Windows), `~/Library/Application Support/strikecut/models` (macOS), or `~/.local/share/strikecut/models` (Linux). Set `STRIKECUT_MODEL_MIRROR` to download from a mirror of [whisper.cpp's models](https://huggingface.co/ggerganov/whisper.cpp).

## How it works

Speech models place word boundaries loosely; even whisper's DTW-aligned timestamps land 0.1 to 0.2s late, which would leave half an "um" audible after a cut. So Strikecut measures the real pauses in the audio and snaps each word boundary to them. Cuts land in silence, not mid-syllable.

```
media ──ffmpeg──▶ 16 kHz wav ──whisper.cpp (DTW)──▶ words ──snap to measured silences──▶ talk.mp4.strikecut.json
                                                                           │  you delete words
                                                                           ▼
             output ◀──ffmpeg select/aselect (one pass)── keep-spans (frame-snapped)
```

| Crate | What it does |
|---|---|
| [`crates/core`](crates/core) | Transcript model, cut-list math, ffmpeg export, captions, transcription |
| [`crates/cli`](crates/cli) | The `strikecut` command |
| [`app`](app) | Tauri desktop app (Rust backend + vanilla TypeScript UI) |

## Roadmap

- [x] One-click model download in the app and CLI
- [ ] Bundled ffmpeg and prebuilt binaries for Windows, macOS, and Linux
- [ ] Studio sound (local noise removal with DeepFilterNet)
- [ ] Speaker labels and per-speaker editing
- [ ] MCP server so agents can edit recordings directly
- [ ] Burned-in, styled captions for social clips
- [ ] Re-record a corrected word in your own voice with local TTS

## Contributing

Issues and PRs are welcome. Run the tests with `cargo test --workspace`.

## License

[MIT](LICENSE)
