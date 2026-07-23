pub mod manager;
pub mod storage;

use chrono::{DateTime, Local};

use crate::stt::{Segment, Transcript};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Idle,
    Recording,
    Paused,
    Finished,
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Status::Idle => "IDLE",
            Status::Recording => "RECORDING",
            Status::Paused => "PAUSED",
            Status::Finished => "FINISHED",
        })
    }
}

#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    pub name: String,
    pub start_time: DateTime<Local>,
    pub end_time: Option<DateTime<Local>>,
    pub status: Status,
    pub transcript: Transcript,
}

impl Session {
    pub fn new(name: &str) -> Session {
        let now = Local::now();
        Session {
            id: now
                .timestamp_nanos_opt()
                .unwrap_or_else(|| now.timestamp_millis())
                .to_string(),
            name: name.to_string(),
            start_time: now,
            end_time: None,
            status: Status::Idle,
            transcript: Transcript::default(),
        }
    }

    /// `YYYY-MM-DD_HH-MM-SS[_sanitized-name]` (local time).
    pub fn folder_name(&self) -> String {
        let ts = self.start_time.format("%Y-%m-%d_%H-%M-%S");
        if self.name.is_empty() {
            ts.to_string()
        } else {
            let sanitized: String = self
                .name
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            format!("{ts}_{sanitized}")
        }
    }

    pub fn duration_secs(&self) -> f64 {
        let end = self.end_time.unwrap_or_else(Local::now);
        (end - self.start_time).num_milliseconds() as f64 / 1000.0
    }

    pub fn duration_formatted(&self) -> String {
        let total = self.duration_secs().max(0.0) as u64;
        format!("{:02}:{:02}:{:02}", total / 3600, (total % 3600) / 60, total % 60)
    }

    pub fn to_metadata(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut m = serde_json::Map::new();
        m.insert("id".into(), self.id.clone().into());
        m.insert("name".into(), self.name.clone().into());
        m.insert("start_time".into(), self.start_time.to_rfc3339().into());
        m.insert(
            "end_time".into(),
            self.end_time.unwrap_or(self.start_time).to_rfc3339().into(),
        );
        m.insert("duration_secs".into(), self.duration_secs().into());
        m.insert("word_count".into(), self.transcript.word_count().into());
        m.insert("speaker_count".into(), self.transcript.speakers().len().into());
        m
    }
}

/// Events emitted by the session manager toward the UI (or headless consumer).
#[derive(Debug, Clone)]
pub enum SessionEvent {
    Segment(Segment),
    Status(Status),
    Error(String),
    AudioLevel(f64),
    Notice(String),
}

/// A stored session as shown in the home list.
#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub folder: String,
    pub name: String,
    pub title: String,
    /// Loaded from metadata.json; not currently displayed (Go parity).
    #[allow(dead_code)]
    pub summary: String,
    pub start_time: Option<DateTime<Local>>,
    pub duration_secs: f64,
    pub word_count: u64,
    pub speaker_count: u64,
}
