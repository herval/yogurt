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

/// ElevenLabs keyterm limits: at most 1000 terms, 50 characters each.
const MAX_KEYTERMS: usize = 1000;
const MAX_KEYTERM_LEN: usize = 50;

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

    /// Glossary terms for STT keyterm biasing: one per non-empty, non-comment
    /// line, trimmed and deduped, clamped to the provider's length/count caps.
    pub fn keyterms(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for line in self.glossary.lines() {
            let term = line.trim();
            if term.is_empty() || term.starts_with('#') {
                continue;
            }
            let term = if term.chars().count() > MAX_KEYTERM_LEN {
                term.chars().take(MAX_KEYTERM_LEN).collect()
            } else {
                term.to_string()
            };
            if !out.iter().any(|t| t == &term) {
                out.push(term);
            }
            if out.len() >= MAX_KEYTERMS {
                break;
            }
        }
        out
    }

    /// Instruction block injected into LLM prompts. `None` when the glossary is
    /// empty, so prompts stay unchanged for users who never set one.
    pub fn llm_prompt(&self) -> Option<String> {
        let terms = self.keyterms();
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
    fn long_terms_clamped_to_50_chars() {
        let long = "a".repeat(80);
        let s = Settings { glossary: long };
        assert_eq!(s.keyterms()[0].chars().count(), MAX_KEYTERM_LEN);
    }
}
