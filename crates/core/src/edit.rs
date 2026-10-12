//! Turning an edited transcript into a cut list ("keep spans") over the source media.

use serde::{Deserialize, Serialize};

use crate::transcript::Word;

/// A half-open time range `[start, end)` in seconds of the source media.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Span {
    pub start: f64,
    pub end: f64,
}

impl Span {
    pub fn duration(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CutOptions {
    /// Seconds kept after the end of the last word before a cut, so trailing consonants aren't clipped.
    pub padding: f64,
    /// Pauses longer than this many seconds are shortened to exactly this length. `None` keeps natural pauses.
    pub max_pause: Option<f64>,
    /// Spans separated by less than this are merged, and spans shorter than this are dropped.
    pub min_span: f64,
}

impl Default for CutOptions {
    fn default() -> Self {
        Self { padding: 0.06, max_pause: None, min_span: 0.03 }
    }
}

/// Compute the parts of the source media to keep, given the transcript's deleted words and pause settings.
///
/// Rules:
/// * Kept words separated only by a pause stay in one span, unless the pause exceeds `max_pause`,
///   in which case the pause is shortened to `max_pause`.
/// * Before a deleted run, the natural pause is kept (capped at `max_pause`); after it, only `padding` lead-in is kept.
/// * Leading and trailing silence is kept unless `max_pause` is set.
pub fn keep_spans(words: &[Word], duration: f64, opts: &CutOptions) -> Vec<Span> {
    let duration = duration.max(words.last().map_or(0.0, |w| w.end));
    if words.is_empty() {
        return if duration > 0.0 { vec![Span { start: 0.0, end: duration }] } else { vec![] };
    }

    let cap = opts.max_pause.unwrap_or(f64::INFINITY);
    let half = opts.max_pause.map(|m| m / 2.0);
    let long_pause = |a: &Word, b: &Word| b.start - a.end > cap;
    let n = words.len();

    let mut spans = Vec::new();
    let mut i = 0;
    while i < n {
        if words[i].deleted {
            i += 1;
            continue;
        }

        let start = if i == 0 {
            half.map_or(0.0, |h| words[0].start - h)
        } else if words[i - 1].deleted {
            (words[i].start - opts.padding).max(words[i - 1].end)
        } else {
            // Split because the pause before this word was too long.
            words[i].start - half.unwrap_or(0.0)
        };

        let mut j = i;
        while j + 1 < n && !words[j + 1].deleted && !long_pause(&words[j], &words[j + 1]) {
            j += 1;
        }

        let end = if j == n - 1 {
            half.map_or(duration, |h| words[j].end + h)
        } else if words[j + 1].deleted {
            // The kept pause plus the lead-in of the next kept word must still fit within max_pause.
            let gap = (words[j + 1].start - words[j].end).max(0.0);
            words[j].end + gap.min((cap - opts.padding).max(0.0))
        } else {
            words[j].end + half.unwrap_or(0.0)
        };

        spans.push(Span { start: start.clamp(0.0, duration), end: end.clamp(0.0, duration) });
        i = j + 1;
    }

    normalize(spans, opts.min_span)
}

fn normalize(spans: Vec<Span>, min_span: f64) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::with_capacity(spans.len());
    for s in spans {
        match out.last_mut() {
            Some(last) if s.start - last.end < min_span => last.end = last.end.max(s.end),
            _ => out.push(s),
        }
    }
    out.retain(|s| s.duration() >= min_span);
    out
}

/// Total length of the edited output.
pub fn output_duration(spans: &[Span]) -> f64 {
    spans.iter().map(Span::duration).sum()
}

/// Map a time in the source media to the edited output's timeline. `None` if that moment was cut.
pub fn source_to_output(t: f64, spans: &[Span]) -> Option<f64> {
    let mut offset = 0.0;
    for s in spans {
        if t < s.start {
            return None;
        }
        if t <= s.end {
            return Some(offset + (t - s.start));
        }
        offset += s.duration();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(spec: &[(f64, f64, bool)]) -> Vec<Word> {
        spec.iter().enumerate().map(|(i, &(s, e, d))| Word { deleted: d, ..Word::new(format!("w{i}"), s, e) }).collect()
    }

    fn approx(spans: &[Span], expected: &[(f64, f64)]) {
        assert_eq!(spans.len(), expected.len(), "{spans:?}");
        for (s, (a, b)) in spans.iter().zip(expected) {
            assert!((s.start - a).abs() < 1e-9 && (s.end - b).abs() < 1e-9, "{spans:?} != {expected:?}");
        }
    }

    #[test]
    fn no_edits_keeps_everything() {
        let w = words(&[(0.5, 1.0, false), (1.2, 1.8, false)]);
        approx(&keep_spans(&w, 3.0, &CutOptions::default()), &[(0.0, 3.0)]);
    }

    #[test]
    fn deleting_a_middle_word_cuts_it() {
        // "hello um world": pause of 0.2 before "um" is kept, 0.06 lead-in before "world".
        let w = words(&[(0.0, 0.5, false), (0.7, 1.0, true), (1.5, 2.0, false)]);
        approx(&keep_spans(&w, 2.0, &CutOptions::default()), &[(0.0, 0.7), (1.44, 2.0)]);
    }

    #[test]
    fn long_pauses_are_shortened() {
        let w = words(&[(1.0, 1.5, false), (4.5, 5.0, false)]);
        let opts = CutOptions { max_pause: Some(0.4), ..Default::default() };
        // Leading/trailing silence trimmed to 0.2; 3s pause becomes 0.2 + 0.2 = 0.4.
        approx(&keep_spans(&w, 8.0, &opts), &[(0.8, 1.7), (4.3, 5.2)]);
    }

    #[test]
    fn pause_around_a_cut_respects_max_pause() {
        // "hello [1s] um [1s] world" with max_pause 0.4: 0.34 kept before the cut + 0.06 lead-in = 0.4.
        let w = words(&[(0.0, 0.5, false), (1.5, 1.8, true), (2.8, 3.3, false)]);
        let opts = CutOptions { max_pause: Some(0.4), ..Default::default() };
        let spans = keep_spans(&w, 3.3, &opts);
        approx(&spans, &[(0.0, 0.84), (2.74, 3.3)]);
        let pause = (spans[0].end - 0.5) + (2.8 - spans[1].start);
        assert!((pause - 0.4).abs() < 1e-9);
    }

    #[test]
    fn deleting_leading_words() {
        let w = words(&[(0.0, 0.4, true), (0.5, 0.9, true), (1.0, 1.5, false)]);
        approx(&keep_spans(&w, 2.0, &CutOptions::default()), &[(0.94, 2.0)]);
    }

    #[test]
    fn deleting_everything_yields_nothing() {
        let w = words(&[(0.0, 0.4, true), (0.5, 0.9, true)]);
        assert!(keep_spans(&w, 2.0, &CutOptions::default()).is_empty());
    }

    #[test]
    fn maps_source_time_to_output() {
        let spans = [Span { start: 0.0, end: 1.0 }, Span { start: 2.0, end: 3.0 }];
        assert_eq!(source_to_output(0.5, &spans), Some(0.5));
        assert_eq!(source_to_output(1.5, &spans), None);
        assert_eq!(source_to_output(2.5, &spans), Some(1.5));
        assert_eq!(output_duration(&spans), 2.0);
    }
}
