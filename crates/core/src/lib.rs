//! Strikecut core: edit audio and video by editing a transcript.
//!
//! The pipeline is:
//! 1. [`transcribe`] media into a [`Transcript`] of timestamped words.
//! 2. Mark words as deleted (by hand, [`filler::mark_fillers`], or [`project::Project::delete_phrase`]).
//! 3. Turn the transcript into a cut list with [`edit::keep_spans`].
//! 4. Render the cut list with [`export::export`] and optionally write captions with [`srt`].

pub mod align;
pub mod edit;
pub mod export;
pub mod filler;
pub mod media;
pub mod models;
pub mod project;
pub mod srt;
pub mod transcribe;
pub mod transcript;

pub use edit::{keep_spans, CutOptions, Span};
pub use project::Project;
pub use transcript::{Transcript, Word};
