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

    pub fn save_chat(&self, folder: &Path, msgs: &[ChatMessage]) -> Result<()> {
        fs::write(folder.join("chat.json"), serde_json::to_string_pretty(msgs)?)?;
        Ok(())
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
