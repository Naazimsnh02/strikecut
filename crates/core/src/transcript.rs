use serde::{Deserialize, Serialize};

/// A single transcribed word with its position in the source media, in seconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Word {
    pub text: String,
    pub start: f64,
    pub end: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<u32>,
    /// Deleted words are cut from the export.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deleted: bool,
}

impl Word {
    pub fn new(text: impl Into<String>, start: f64, end: f64) -> Self {
        Self {
            text: text.into(),
            start,
            end,
            speaker: None,
            deleted: false,
        }
    }

    /// Lowercased text with surrounding punctuation removed, for matching.
    pub fn normalized(&self) -> String {
        normalize(&self.text)
    }
}

pub(crate) fn normalize(s: &str) -> String {
    s.trim()
        .trim_matches(|c: char| !c.is_alphanumeric() && c != '\'')
        .to_lowercase()
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub words: Vec<Word>,
}

impl Transcript {
    pub fn new(words: Vec<Word>) -> Self {
        Self {
            language: None,
            words,
        }
    }

    /// Plain text of the words that survive the edit.
    pub fn kept_text(&self) -> String {
        join(self.words.iter().filter(|w| !w.deleted))
    }

    /// Text with deleted words wrapped in `~~strikethrough~~`, handy for reviewing edits in a terminal or by an agent.
    pub fn marked_text(&self) -> String {
        self.words
            .chunk_by(|a, b| a.deleted == b.deleted)
            .map(|run| {
                let text = join(run.iter());
                if run[0].deleted {
                    format!("~~{text}~~")
                } else {
                    text
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Fix ordering and overlaps that transcription engines occasionally produce.
    pub fn sanitize(&mut self) {
        self.words.retain(|w| !w.text.trim().is_empty() && w.end >= w.start);
        self.words
            .sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal));
        for i in 1..self.words.len() {
            let prev_end = self.words[i - 1].end;
            let w = &mut self.words[i];
            if w.start < prev_end {
                w.start = prev_end;
                if w.end < w.start {
                    w.end = w.start;
                }
            }
        }
    }
}

fn join<'a>(words: impl Iterator<Item = &'a Word>) -> String {
    words.map(|w| w.text.trim()).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(words: &[(&str, bool)]) -> Transcript {
        Transcript::new(
            words
                .iter()
                .enumerate()
                .map(|(i, (s, d))| Word {
                    deleted: *d,
                    ..Word::new(*s, i as f64, i as f64 + 0.5)
                })
                .collect(),
        )
    }

    #[test]
    fn marked_text_wraps_deleted_runs() {
        let tr = t(&[("so", true), ("um", true), ("hello", false), ("there", false), ("uh", true), ("friend", false)]);
        assert_eq!(tr.marked_text(), "~~so um~~ hello there ~~uh~~ friend");
        assert_eq!(tr.kept_text(), "hello there friend");
    }

    #[test]
    fn normalize_strips_punctuation() {
        assert_eq!(normalize(" Um,"), "um");
        assert_eq!(normalize("don't."), "don't");
    }

    #[test]
    fn sanitize_orders_and_clamps() {
        let mut tr = Transcript::new(vec![Word::new("b", 1.0, 2.0), Word::new("a", 0.0, 1.5), Word::new(" ", 3.0, 4.0)]);
        tr.sanitize();
        assert_eq!(tr.words.len(), 2);
        assert_eq!(tr.words[0].text, "a");
        assert_eq!(tr.words[1].start, 1.5);
    }
}
