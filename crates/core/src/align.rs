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
}

impl Default for AlignOptions {
    fn default() -> Self {
        Self { min_silence: 0.06, max_shift: 0.5 }
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
pub fn snap_to_silences(words: &mut [Word], silences: &[(f64, f64)], max_shift: f64) {
    let n = words.len();
    if n == 0 {
        return;
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
    for &(b, g) in &assigned {
        let (s0, s1) = silences[g];
        if b < n {
            words[b].start = s1;
        }
        if b > 0 {
            words[b - 1].end = s0;
        }
        snapped[b] = true;
    }
    // Between words with no pause, one word ends where the next begins.
    for b in 1..n {
        if !snapped[b] {
            words[b - 1].end = words[b].start;
        }
    }
    // Keep times ordered even if the model's estimates were inconsistent.
    for i in 0..n {
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
        let t: Vec<(f64, f64)> = words.iter().map(|w| (w.start, w.end)).collect();
        assert_eq!(t, [(0.76, 1.04), (1.95, 2.16), (2.60, 3.02), (3.02, 3.02)]);
    }

    #[test]
    fn leaves_words_alone_without_nearby_silence() {
        let mut words = vec![Word::new("a", 1.0, 1.4), Word::new("b", 1.4, 1.9)];
        snap_to_silences(&mut words, &[(5.0, 6.0)], 0.5);
        assert_eq!((words[0].start, words[1].start), (1.0, 1.4));
    }
}
