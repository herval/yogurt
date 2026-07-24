//! User-editable glossary of domain vocabulary (names, product names, jargon).
//!
//! Free-form text, one term or phrase per line (blank lines and `#` comments
//! ignored). It is fed to two places to correct words that generic models get
//! wrong:
//!   - STT: as ElevenLabs Scribe v2 `keyterms`, which bias transcription
//!     toward these spellings at the source.
//!   - LLM: prepended to the title/summary, speaker-identification, and chat
//!     prompts so derived text uses the same canonical spellings.
//!
//! Persisted at ~/.yogurt/glossary.txt.

use std::path::PathBuf;

/// ElevenLabs keyterm limits: at most 1000 terms, 50 characters and 4 spaces
/// (i.e. 5 words) each. Terms over these limits are dropped from the STT
/// keyterm list (a 400 otherwise) but kept for the LLM, which handles phrases.
const MAX_KEYTERMS: usize = 1000;
const MAX_KEYTERM_LEN: usize = 50;
const MAX_KEYTERM_SPACES: usize = 4;

#[derive(Debug, Clone, Default)]
pub struct Settings {
    /// Raw glossary text, exactly as the user typed it.
    pub glossary: String,
}

pub fn glossary_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".yogurt")
        .join("glossary.txt")
}

impl Settings {
    pub fn load() -> Settings {
        Settings {
            glossary: std::fs::read_to_string(glossary_path()).unwrap_or_default(),
        }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = glossary_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, &self.glossary)?;
        Ok(())
    }

    /// The full glossary vocabulary: one entry per non-empty, non-comment line,
    /// whitespace-normalized and deduped. No length/word caps — this is what the
    /// LLM sees, and it handles multi-word phrases fine.
    pub fn terms(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for line in self.glossary.lines() {
            let term = line.trim();
            if term.is_empty() || term.starts_with('#') {
                continue;
            }
            // Collapse internal whitespace runs so the space count below matches
            // ElevenLabs' post-normalization word count.
            let term = term.split_whitespace().collect::<Vec<_>>().join(" ");
            if !out.iter().any(|t| t == &term) {
                out.push(term);
            }
        }
        out
    }

    /// Glossary terms valid as ElevenLabs STT keyterms: dropped if over 50 chars
    /// or 5 words (ElevenLabs 400s the whole request otherwise), capped at 1000.
    pub fn keyterms(&self) -> Vec<String> {
        self.terms()
            .into_iter()
            .filter(|t| {
                t.chars().count() <= MAX_KEYTERM_LEN
                    && t.matches(' ').count() <= MAX_KEYTERM_SPACES
            })
            .take(MAX_KEYTERMS)
            .collect()
    }

    /// Instruction block injected into LLM prompts. `None` when the glossary is
    /// empty, so prompts stay unchanged for users who never set one.
    pub fn llm_prompt(&self) -> Option<String> {
        let terms = self.terms();
        if terms.is_empty() {
            return None;
        }
        let list = terms
            .iter()
            .map(|t| format!("- {t}"))
            .collect::<Vec<_>>()
            .join("\n");
        Some(format!(
            "The transcript may contain names, product names, and domain-specific \
             terms that speech-to-text can mis-spell. Below is a glossary of their \
             correct spellings. When a term that clearly refers to one of these \
             appears (including phonetically similar mis-transcriptions), use the \
             glossary spelling exactly:\n{list}"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyterms_trims_dedupes_and_skips_comments() {
        let s = Settings {
            glossary: "  Telepatia \n# a comment\n\nDatadog\nTelepatia\n".into(),
        };
        assert_eq!(s.keyterms(), vec!["Telepatia", "Datadog"]);
    }

    #[test]
    fn empty_glossary_yields_no_prompt() {
        let s = Settings {
            glossary: "\n  \n# only comments\n".into(),
        };
        assert!(s.keyterms().is_empty());
        assert!(s.llm_prompt().is_none());
    }

    #[test]
    fn keyterms_drop_over_length_and_wordy_but_llm_keeps_them() {
        let s = Settings {
            glossary: format!(
                "Datadog\n{}\nthis phrase has way too many words for a keyterm\n",
                "a".repeat(80)
            ),
        };
        // STT keyterms exclude the 80-char term and the 9-word phrase.
        assert_eq!(s.keyterms(), vec!["Datadog"]);
        // The LLM prompt still lists all three (phrases are fine there).
        let prompt = s.llm_prompt().unwrap();
        assert!(prompt.contains("Datadog"));
        assert!(prompt.contains("too many words"));
    }

    #[test]
    fn keyterms_allow_up_to_five_words() {
        let s = Settings {
            glossary: "one two three four five\nsix seven eight nine ten eleven".into(),
        };
        // 5 words (4 spaces) ok; 6 words (5 spaces) dropped.
        assert_eq!(s.keyterms(), vec!["one two three four five"]);
    }
}
