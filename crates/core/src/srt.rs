//! SubRip captions for the edited output, timed on the new (post-cut) timeline.

use std::fmt::Write;

use crate::edit::{source_to_output, Span};
use crate::transcript::Word;

#[derive(Debug, Clone)]
pub struct CaptionOptions {
    pub max_words: usize,
    pub max_seconds: f64,
}

impl Default for CaptionOptions {
    fn default() -> Self {
        Self { max_words: 7, max_seconds: 3.0 }
    }
}

struct Cue {
    start: f64,
    end: f64,
    text: Vec<String>,
}

pub fn to_srt(words: &[Word], spans: &[Span], opts: &CaptionOptions) -> String {
    let mut cues: Vec<Cue> = Vec::new();
    let mut cur: Option<Cue> = None;

    for w in words.iter().filter(|w| !w.deleted) {
        let (Some(start), Some(end)) = (
            source_to_output(w.start, spans),
            source_to_output(w.end, spans).or_else(|| source_to_output(w.start, spans)),
        ) else {
            continue;
        };
        let text = w.text.trim().to_string();

        if let Some(c) = cur.as_mut() {
            if c.text.len() < opts.max_words && end - c.start <= opts.max_seconds {
                c.end = end;
                c.text.push(text);
                if ends_sentence(c.text.last().unwrap()) {
                    cues.extend(cur.take());
                }
                continue;
            }
            cues.extend(cur.take());
        }
        let sentence_end = ends_sentence(&text);
        cur = Some(Cue { start, end, text: vec![text] });
        if sentence_end {
            cues.extend(cur.take());
        }
    }
    cues.extend(cur);

    let mut out = String::new();
    for (i, c) in cues.iter().enumerate() {
        let _ = writeln!(out, "{}\n{} --> {}\n{}\n", i + 1, timestamp(c.start), timestamp(c.end), c.text.join(" "));
    }
    out
}

fn ends_sentence(s: &str) -> bool {
    s.ends_with(['.', '?', '!'])
}

fn timestamp(t: f64) -> String {
    let ms = (t.max(0.0) * 1000.0).round() as u64;
    format!("{:02}:{:02}:{:02},{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retimes_captions_after_cuts() {
        let mut words = vec![
            Word::new("Hello", 0.0, 0.5),
            Word::new("um", 1.0, 1.5),
            Word::new("world.", 2.0, 2.5),
            Word::new("Bye.", 3.0, 3.5),
        ];
        words[1].deleted = true;
        let spans = [Span { start: 0.0, end: 1.0 }, Span { start: 1.9, end: 3.6 }];
        let srt = to_srt(&words, &spans, &CaptionOptions::default());
        assert_eq!(srt, "1\n00:00:00,000 --> 00:00:01,600\nHello world.\n\n2\n00:00:02,100 --> 00:00:02,600\nBye.\n\n");
    }

    #[test]
    fn formats_timestamps() {
        assert_eq!(timestamp(3725.5), "01:02:05,500");
    }
}
