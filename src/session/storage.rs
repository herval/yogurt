use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use serde_json::Value;

use crate::audio::wav::write_wav;
use crate::llm::client::ChatMessage;

use super::{Session, SessionSummary};

pub struct Storage {
    pub base_dir: PathBuf,
}

impl Storage {
    pub fn new(base_dir: PathBuf) -> Storage {
        Storage { base_dir }
    }

    /// Write the full session folder. Returns the folder path.
    pub fn save(&self, sess: &Session, pcm: &[u8], sample_rate: u32, channels: u16) -> Result<PathBuf> {
        let folder = self.base_dir.join(sess.folder_name());
        fs::create_dir_all(&folder).context("create session folder")?;

        write_wav(&folder.join("audio.wav"), pcm, sample_rate, channels)?;
        fs::write(folder.join("transcript.txt"), sess.transcript.to_plain_text())?;

        #[derive(serde::Serialize)]
        struct TranscriptJson<'a> {
            segments: &'a [crate::stt::Segment],
        }
        fs::write(
            folder.join("transcript.json"),
            serde_json::to_string_pretty(&TranscriptJson {
                segments: &sess.transcript.segments,
            })?,
        )?;
        fs::write(
            folder.join("metadata.json"),
            serde_json::to_string_pretty(&Value::Object(sess.to_metadata()))?,
        )?;
        Ok(folder)
    }

    /// Add title+summary to metadata.json (preserving unknown keys) + summary.md.
    pub fn save_meta(&self, folder: &Path, title: &str, summary: &str) -> Result<()> {
        let meta_path = folder.join("metadata.json");
        let mut meta: serde_json::Map<String, Value> = fs::read_to_string(&meta_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        meta.insert("title".into(), title.into());
        meta.insert("summary".into(), summary.into());
        fs::write(&meta_path, serde_json::to_string_pretty(&Value::Object(meta))?)?;
        fs::write(folder.join("summary.md"), format!("# {title}\n\n{summary}\n"))?;
        Ok(())
    }

    /// Rewrite a saved session's speaker labels with identified names.
    /// Returns how many segments were relabeled.
    pub fn apply_speaker_names(
        &self,
        folder: &Path,
        names: &std::collections::HashMap<String, String>,
    ) -> Result<usize> {
        #[derive(serde::Deserialize, serde::Serialize)]
        struct TranscriptJson {
            segments: Vec<crate::stt::Segment>,
        }
        let path = folder.join("transcript.json");
        let mut t: TranscriptJson = serde_json::from_str(&fs::read_to_string(&path)?)?;
        let mut changed = 0;
        for seg in &mut t.segments {
            if let Some(name) = names.get(&seg.speaker) {
                seg.speaker = name.clone();
                changed += 1;
            }
        }
        if changed == 0 {
            return Ok(0);
        }
        fs::write(&path, serde_json::to_string_pretty(&t)?)?;

        let mut transcript = crate::stt::Transcript::default();
        transcript.replace_segments(t.segments);
        fs::write(folder.join("transcript.txt"), transcript.to_plain_text())?;

        // Record the mapping in metadata for traceability.
        let meta_path = folder.join("metadata.json");
        let mut meta: serde_json::Map<String, Value> = fs::read_to_string(&meta_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        meta.insert(
            "speaker_names".into(),
            serde_json::to_value(names).unwrap_or_default(),
        );
        fs::write(&meta_path, serde_json::to_string_pretty(&Value::Object(meta))?)?;
        Ok(changed)
    }

    pub fn save_chat(&self, folder: &Path, msgs: &[ChatMessage]) -> Result<()> {
        fs::write(folder.join("chat.json"), serde_json::to_string_pretty(msgs)?)?;
        Ok(())
    }

    /// Missing or corrupt chat.json reads as an empty conversation.
    pub fn load_chat(&self, folder: &Path) -> Vec<ChatMessage> {
        fs::read_to_string(folder.join("chat.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// The global (no-session) chat lives at base_dir/chat.json; list_sessions
    /// only looks at directories, so it never shows up as a session.
    pub fn save_global_chat(&self, msgs: &[ChatMessage]) -> Result<()> {
        fs::create_dir_all(&self.base_dir).context("create sessions dir")?;
        self.save_chat(&self.base_dir, msgs)
    }

    pub fn load_global_chat(&self) -> Vec<ChatMessage> {
        self.load_chat(&self.base_dir)
    }

    /// Newest-first list of sessions with parseable metadata.json.
    pub fn list_sessions(&self) -> Vec<SessionSummary> {
        let Ok(entries) = fs::read_dir(&self.base_dir) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        names.sort();
        names.reverse();

        names
            .into_iter()
            .filter_map(|name| {
                let folder = self.base_dir.join(&name);
                let meta: serde_json::Map<String, Value> =
                    serde_json::from_str(&fs::read_to_string(folder.join("metadata.json")).ok()?)
                        .ok()?;
                let str_of = |k: &str| {
                    meta.get(k)
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string()
                };
                let session_name = {
                    let n = str_of("name");
                    if n.is_empty() { name.clone() } else { n }
                };
                Some(SessionSummary {
                    folder: folder.to_string_lossy().to_string(),
                    name: session_name,
                    title: str_of("title"),
                    summary: str_of("summary"),
                    start_time: meta
                        .get("start_time")
                        .and_then(|v| v.as_str())
                        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                        .map(|d| d.with_timezone(&Local)),
                    duration_secs: meta.get("duration_secs").and_then(|v| v.as_f64()).unwrap_or(0.0),
                    word_count: meta.get("word_count").and_then(|v| v.as_u64()).unwrap_or(0),
                    speaker_count: meta.get("speaker_count").and_then(|v| v.as_u64()).unwrap_or(0),
                })
            })
            .collect()
    }

    pub fn load_transcript(&self, folder: &Path) -> String {
        fs::read_to_string(folder.join("transcript.txt"))
            .unwrap_or_else(|_| "(transcript not available)".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Local;

    #[test]
    fn apply_speaker_names_rewrites_files() {
        let dir = std::env::temp_dir().join(format!("yogurt-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let storage = Storage::new(dir.clone());

        let seg = |speaker: &str, text: &str| crate::stt::Segment {
            text: text.into(),
            speaker: speaker.into(),
            start_time: 0.0,
            end_time: 1.0,
            confidence: 1.0,
            is_final: true,
            created_at: Local::now(),
        };
        let mut sess = crate::session::Session::new("t");
        sess.transcript.replace_segments(vec![
            seg("You", "hi"),
            seg("A", "hello"),
            seg("B", "hey"),
        ]);
        let folder = storage.save(&sess, &[0u8; 64000], 16000, 1).unwrap();

        let mut names = std::collections::HashMap::new();
        names.insert("A".to_string(), "Daniel".to_string());
        let changed = storage.apply_speaker_names(&folder, &names).unwrap();
        assert_eq!(changed, 1);

        let txt = std::fs::read_to_string(folder.join("transcript.txt")).unwrap();
        assert!(txt.contains("Daniel:"), "renamed label in txt: {txt}");
        assert!(txt.contains("Speaker B:"), "unmapped speaker keeps letter");
        assert!(txt.contains("You:"), "You untouched");

        let meta: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(folder.join("metadata.json")).unwrap())
                .unwrap();
        assert_eq!(meta["speaker_names"]["A"], "Daniel");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn chat_msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.into(),
            content: content.into(),
        }
    }

    #[test]
    fn chat_round_trip_and_forgiving_load() {
        let dir = std::env::temp_dir().join(format!("yogurt-chat-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let storage = Storage::new(dir.clone());

        let msgs = vec![chat_msg("user", "hi"), chat_msg("assistant", "hello")];
        storage.save_chat(&dir, &msgs).unwrap();

        // Serialized with capitalized keys (Go parity).
        let raw = std::fs::read_to_string(dir.join("chat.json")).unwrap();
        assert!(raw.contains("\"Role\""), "capitalized keys: {raw}");

        let loaded = storage.load_chat(&dir);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].role, "user");
        assert_eq!(loaded[1].content, "hello");

        // Missing folder and garbage bytes both read as empty.
        assert!(storage.load_chat(&dir.join("nope")).is_empty());
        std::fs::write(dir.join("chat.json"), "{not json").unwrap();
        assert!(storage.load_chat(&dir).is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn global_chat_creates_base_dir() {
        let dir = std::env::temp_dir().join(format!("yogurt-globalchat-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let storage = Storage::new(dir.clone());

        assert!(storage.load_global_chat().is_empty());
        let msgs = vec![chat_msg("user", "hey")];
        storage.save_global_chat(&msgs).unwrap();
        assert_eq!(storage.load_global_chat().len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
