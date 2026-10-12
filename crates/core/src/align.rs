//! Tightening word timestamps against the audio itself.
//!
//! Speech models place word boundaries loosely: whisper's default timestamps can be off by half a second, and even
//! DTW-aligned onsets land ~0.1–0.2s late. Cutting at those times leaves fragments of deleted words audible
//! ("um" becomes "m"). Silences, though, can be measured exactly. So we find the real pauses in the audio and snap
//! the nearest word boundary to each one: the word before ends where the silence starts, the word after begins where
//! it ends. Boundaries inside continuous speech keep the model's estimate.

use crate::transcript::Word;

/// Analysis frame length in seconds.
pub const FRAME: f64 = 0.01;

#[derive(Debug, Clone)]
pub struct AlignOptions {
    /// Shortest gap treated as a pause between words.
    pub min_silence: f64,
    /// How far (seconds) a model timestamp may be moved to reach a silence edge.
    pub max_shift: f64,
    /// How far (seconds) a boundary inside continuous speech may move to reach the quietest point.
    pub dip_window: f64,
}

impl Default for AlignOptions {
    fn default() -> Self {
        Self { min_silence: 0.06, max_shift: 0.5, dip_window: 0.06 }
    }
}

/// Loudness in dBFS of each 10 ms frame of 16-bit mono audio.
pub fn frame_db(samples: &[i16], sample_rate: u32) -> Vec<f32> {
    let n = ((sample_rate as f64 * FRAME) as usize).max(1);
    samples
        .chunks(n)
        .map(|c| {
            let ms = c.iter().map(|&s| (s as f64 / 32768.0).powi(2)).sum::<f64>() / c.len() as f64;
            (10.0 * ms.max(1e-10).log10()) as f32
        })
        .collect()
}

/// Silent stretches `(start, end)` in seconds. The threshold adapts to the recording: a quarter of the way from the
/// noise floor to typical speech level, but never more than 30 dB below speech.
pub fn silences(db: &[f32], min_len: f64) -> Vec<(f64, f64)> {
    if db.is_empty() {
        return vec![];
    }
    let mut sorted = db.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let pct = |p: f64| sorted[((sorted.len() - 1) as f64 * p) as usize];
    let (floor, speech) = (pct(0.10), pct(0.90));
    let threshold = (floor + (speech - floor) * 0.25).max(speech - 30.0);

    let min_frames = (min_len / FRAME).round() as usize;
    let mut out = Vec::new();
    let mut run_start = None;
    for (i, &d) in db.iter().chain(std::iter::once(&f32::INFINITY)).enumerate() {
        match (d < threshold, run_start) {
            (true, None) => run_start = Some(i),
            (false, Some(s)) => {
                if i - s >= min_frames {
                    // Divide rather than multiply by FRAME so times stay exact (0.66, not 0.6600000000000001).
                    let frames_per_sec = (1.0 / FRAME).round();
                    out.push((s as f64 / frames_per_sec, i as f64 / frames_per_sec));
                }
                run_start = None;
            }
            _ => {}
        }
    }
    out
}

/// Snap word boundaries to measured silences. Boundary `b` sits before word `b`; boundary `n` follows the last word.
/// Returns which boundaries landed on a measured pause.
pub fn snap_to_silences(words: &mut [Word], silences: &[(f64, f64)], max_shift: f64) -> Vec<bool> {
    let n = words.len();
    if n == 0 {
        return vec![];
    }
    // Where the model thinks each boundary is. A pause is matched by where speech resumes (its end), except the
    // final boundary, which is matched by where speech stops.
    let distance = |b: usize, (s0, s1): (f64, f64)| {
        if b < n {
            (words[b].start - s1).abs()
        } else {
            (words[n - 1].end - s0).abs()
        }
    };

    let mut candidates: Vec<(f64, usize, usize)> = Vec::new();
    for (g, &sil) in silences.iter().enumerate() {
        for b in 0..=n {
            let d = distance(b, sil);
            if d <= max_shift {
                candidates.push((d, b, g));
            }
        }
    }
    candidates.sort_by(|a, b| a.0.total_cmp(&b.0));

    // Greedy closest-first matching, keeping boundaries and silences in the same order.
    let mut assigned: Vec<(usize, usize)> = Vec::new();
    for (_, b, g) in candidates {
        let consistent = assigned.iter().all(|&(b2, g2)| b != b2 && g != g2 && (b < b2) == (g < g2));
        if consistent {
            assigned.push((b, g));
        }
    }
    assigned.sort();

    let mut snapped = vec![false; n + 1];
    // Per boundary: the model's estimate, where the next word starts, and where the previous word ends.
    let model: Vec<f64> = (0..=n).map(|b| if b < n { words[b].start } else { words[n - 1].end }).collect();
    let model_end: Vec<f64> = (0..=n).map(|b| if b > 0 { words[b - 1].end } else { words[0].start }).collect();
    let mut starts = model.clone();
    let mut ends = model.clone();
    for &(b, g) in &assigned {
        (ends[b], starts[b]) = silences[g];
        snapped[b] = true;
    }

    // Between two snapped boundaries lies a stretch of continuous speech whose true edges are now known. The model's
    // interior boundaries tend to be late by a similar amount, so map them proportionally into the measured stretch
    // instead of trusting their absolute times (which can push a short word past the stretch's end entirely).
    let anchors: Vec<usize> = (0..=n).filter(|&b| snapped[b]).collect();
    for pair in anchors.windows(2) {
        let (a, c) = (pair[0], pair[1]);
        let (from, to) = (starts[a], ends[c]);
        let span = model_end[c].max(model[c - 1]) - model[a];
        for b in a + 1..c {
            let frac = if span > 0.0 { (model[b] - model[a]) / span } else { (b - a) as f64 / (c - a) as f64 };
            let t = from + (to - from) * frac.clamp(0.0, 1.0);
            (starts[b], ends[b]) = (t, t);
        }
    }
    // Before the first and after the last anchor there's only one measured edge: shift by its correction.
    if let (Some(&first), Some(&last)) = (anchors.first(), anchors.last()) {
        let before = if first < n { starts[first] - model[first] } else { ends[first] - model[first] };
        for b in 0..first {
            let t = (model[b] + before).max(0.0);
            (starts[b], ends[b]) = (t, t);
        }
        let after = if last < n { starts[last] - model[last] } else { ends[last] - model[last] };
        for b in last + 1..=n {
            let t = (model[b] + after).max(0.0);
            (starts[b], ends[b]) = (t, t);
        }
    }

    for b in 0..=n {
        if b < n {
            words[b].start = starts[b];
        }
        if b > 0 {
            words[b - 1].end = ends[b];
        }
    }
    order(words);
    snapped
}

/// Move each boundary that isn't at a pause to the quietest 10 ms frame within `window` seconds. Inside continuous
/// speech, the dip between two words is the least audible place to cut.
pub fn snap_to_dips(words: &mut [Word], snapped: &[bool], db: &[f32], window: f64) {
    let n = words.len();
    let reach = (window / FRAME).round() as isize;
    for b in 1..n {
        if snapped.get(b).copied().unwrap_or(false) {
            continue;
        }
        let t = words[b].start;
        let lo = words[b - 1].start.max(t - window);
        let hi = words[b].end.min(t + window);
        let center = (t / FRAME).round() as isize;
        let best = (center - reach..=center + reach)
            .filter(|&i| i >= 0 && (i as usize) < db.len())
            .map(|i| (i, i as f64 * FRAME))
            .filter(|&(_, at)| at > lo && at < hi)
            .min_by(|x, y| {
                db[x.0 as usize].total_cmp(&db[y.0 as usize]).then((x.0 - center).abs().cmp(&(y.0 - center).abs()))
            });
        if let Some((i, _)) = best {
            if db[i as usize] < db[(center.max(0) as usize).min(db.len().saturating_sub(1))] {
                let at = i as f64 / (1.0 / FRAME).round();
                words[b].start = at;
                words[b - 1].end = at;
            }
        }
    }
    order(words);
}

/// Keep times ordered even if the model's estimates were inconsistent.
fn order(words: &mut [Word]) {
    for i in 0..words.len() {
        if i > 0 && words[i].start < words[i - 1].end {
            words[i].start = words[i - 1].end;
        }
        if words[i].end < words[i].start {
            words[i].end = words[i].start;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_silences_with_adaptive_threshold() {
        // 0.3s quiet, 0.5s loud, 0.1s quiet, 0.2s loud, 0.02s quiet (too short), 0.2s loud
        let mut db = vec![-70.0; 30];
        db.extend(vec![-20.0; 50]);
        db.extend(vec![-65.0; 10]);
        db.extend(vec![-22.0; 20]);
        db.extend(vec![-60.0; 2]);
        db.extend(vec![-21.0; 20]);
        let s = silences(&db, 0.06);
        assert_eq!(s.len(), 2);
        assert!((s[0].0 - 0.0).abs() < 1e-9 && (s[0].1 - 0.3).abs() < 1e-9);
        assert!((s[1].0 - 0.8).abs() < 1e-9 && (s[1].1 - 0.9).abs() < 1e-9);
    }

    #[test]
    fn snaps_late_dtw_onsets_to_real_pauses() {
        // Real timings measured from a TTS recording; model onsets are 0.08-0.18s late.
        let mut words = vec![
            Word::new("everyone.", 0.76, 1.36),
            Word::new("Um,", 2.04, 1.76),
            Word::new("today", 2.78, 2.25),
            Word::new("I", 3.02, 2.35),
        ];
        let sil = [(0.0, 0.12), (1.04, 1.95), (2.16, 2.60)];
        snap_to_silences(&mut words, &sil, 0.5);
        // Words outside the matched pauses move by the neighbouring pause's correction (0.09s and 0.18s early).
        assert_times(&words, &[(0.67, 1.04), (1.95, 2.16), (2.60, 2.84), (2.84, 2.84)]);
    }

    #[test]
    fn maps_words_between_pauses_into_the_measured_speech() {
        // "where um if" spoken without pauses, between two measured pauses. The model's interior boundaries are late
        // enough that "if" would start after speech has already stopped (28.60).
        let mut words = vec![
            Word::new("where,", 28.25, 28.43),
            Word::new("um,", 28.43, 28.66),
            Word::new("if", 28.66, 28.76),
            Word::new("the", 28.86, 28.98),
        ];
        let snapped = snap_to_silences(&mut words, &[(28.12, 28.19), (28.60, 28.86)], 0.5);
        assert_eq!(snapped, [true, false, false, true, false]);
        // The stretch 28.19-28.60 is split in the model's proportions (0.18 : 0.23 : 0.10 of 0.51s).
        assert_times(&words[..3], &[(28.19, 28.335), (28.335, 28.52), (28.52, 28.60)]);
        assert!(words[2].end > words[2].start, "no word collapses to zero length");
    }

    #[test]
    fn interior_boundaries_move_to_the_quietest_frame() {
        let mut words = vec![Word::new("a", 0.0, 0.50), Word::new("b", 0.50, 1.0)];
        let mut db = vec![-20.0f32; 100];
        db[46] = -35.0; // dip at 0.46s, within the 0.06s window
        db[30] = -60.0; // quieter, but too far away
        snap_to_dips(&mut words, &[true, false, true], &db, 0.06);
        assert_times(&words, &[(0.0, 0.46), (0.46, 1.0)]);
    }

    fn assert_times(words: &[Word], expected: &[(f64, f64)]) {
        let got: Vec<(f64, f64)> = words.iter().map(|w| (w.start, w.end)).collect();
        assert_eq!(got.len(), expected.len());
        for (g, e) in got.iter().zip(expected) {
            assert!((g.0 - e.0).abs() < 1e-3 && (g.1 - e.1).abs() < 1e-3, "{got:?} != {expected:?}");
        }
    }

    #[test]
    fn leaves_words_alone_without_nearby_silence() {
        let mut words = vec![Word::new("a", 1.0, 1.4), Word::new("b", 1.4, 1.9)];
        snap_to_silences(&mut words, &[(5.0, 6.0)], 0.5);
        assert_eq!((words[0].start, words[1].start), (1.0, 1.4));
    }
}
