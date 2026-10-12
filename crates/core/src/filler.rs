//! Detecting filler words ("um", "uh", ...) so they can be removed in one click.

use crate::transcript::{normalize, Transcript};

/// Single-word fillers that are almost never meaningful speech.
pub const DEFAULT_FILLERS: &[&str] = &[
    "um", "umm", "ummm", "uh", "uhh", "uhm", "uh-huh", "er", "erm", "ah", "ahh", "hmm", "hm", "mm", "mmm",
];

pub fn is_filler(text: &str, fillers: &[&str]) -> bool {
    let n = normalize(text);
    !n.is_empty() && fillers.contains(&n.as_str())
}

/// Mark every filler word as deleted. Returns how many words were newly marked.
pub fn mark_fillers(transcript: &mut Transcript, fillers: &[&str]) -> usize {
    let mut count = 0;
    for w in transcript.words.iter_mut().filter(|w| !w.deleted) {
        if is_filler(&w.text, fillers) {
            w.deleted = true;
            count += 1;
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::Word;

    #[test]
    fn marks_fillers_case_and_punctuation_insensitively() {
        let mut t = Transcript::new(
            ["Um,", "so", "uh", "the", "umbrella", "Hmm."]
                .iter()
                .enumerate()
                .map(|(i, s)| Word::new(*s, i as f64, i as f64 + 0.5))
                .collect(),
        );
        assert_eq!(mark_fillers(&mut t, DEFAULT_FILLERS), 3);
        assert_eq!(t.kept_text(), "so the umbrella");
    }
}
