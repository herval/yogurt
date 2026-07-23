pub mod assemblyai;
pub mod elevenlabs;
#[cfg(feature = "whisper")]
pub mod whisper;

use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    pub text: String,
    pub speaker: String, // "A","B",... or "" unknown
    pub start_time: f64, // seconds
    pub end_time: f64,   // seconds
    pub confidence: f64,
    pub is_final: bool,
    pub created_at: DateTime<Local>,
}

impl Segment {
    pub fn format_timestamp(&self) -> String {
        let total = self.start_time as u64;
        format!("{:02}:{:02}:{:02}", total / 3600, (total % 3600) / 60, total % 60)
    }
}

/// Finalized segments plus at most one in-progress partial.
#[derive(Debug, Default, Clone)]
pub struct Transcript {
    pub segments: Vec<Segment>,
    partial: Option<Segment>,
}

impl Transcript {
    pub fn add_segment(&mut self, seg: Segment) {
        if seg.is_final {
            self.segments.push(seg);
            self.partial = None;
        } else {
            self.partial = Some(seg);
        }
    }

    /// Swap in an authoritative re-transcription (quasi-live batch providers).
    pub fn replace_segments(&mut self, segments: Vec<Segment>) {
        self.segments = segments;
        self.partial = None;
    }

    /// Space-counting heuristic — kept identical to Go so stored word counts match.
    pub fn word_count(&self) -> usize {
        self.segments
            .iter()
            .filter(|s| !s.text.trim().is_empty())
            .map(|s| s.text.matches(' ').count() + 1)
            .sum()
    }

    pub fn speakers(&self) -> Vec<String> {
        let mut seen = Vec::new();
        for s in &self.segments {
            if !s.speaker.is_empty() && !seen.contains(&s.speaker) {
                seen.push(s.speaker.clone());
            }
        }
        seen
    }

    pub fn to_plain_text(&self) -> String {
        let mut out = String::new();
        for s in &self.segments {
            let speaker = if s.speaker.is_empty() {
                "Unknown".to_string()
            } else if s.speaker == "You" {
                "You".to_string()
            } else {
                format!("Speaker {}", s.speaker)
            };
            out.push_str(&format!("[{}] {}:\n{}\n\n", s.format_timestamp(), speaker, s.text));
        }
        out
    }
}

/// Synchronous event callbacks, shared with provider worker threads.
///
/// INVARIANT: batch providers (elevenlabs, whisper) deliver ALL their segments
/// via on_segment inside close(), before it returns — session finish depends
/// on this ordering.
#[derive(Clone)]
pub struct SttCallbacks {
    pub on_segment: Arc<dyn Fn(Segment) + Send + Sync>,
    /// Authoritative re-transcription replacing everything delivered so far
    /// (used by quasi-live batch providers at close). Most providers never
    /// call this.
    pub on_replace: Arc<dyn Fn(Vec<Segment>) + Send + Sync>,
    pub on_error: Arc<dyn Fn(String) + Send + Sync>,
    pub on_connected: Arc<dyn Fn() + Send + Sync>,
    pub on_disconnect: Arc<dyn Fn() + Send + Sync>,
}

pub trait SttClient: Send {
    fn connect(&mut self) -> Result<()>;
    fn send_audio(&self, pcm: &[u8]) -> Result<()>;
    /// Blocking. Batch providers transcribe and fire on_segment here.
    fn close(&mut self) -> Result<()>;
}

pub fn new_stt_client(
    provider: &str,
    api_key: &str,
    sample_rate: u32,
    channels: u16,
    model: &str,
    callbacks: SttCallbacks,
) -> Box<dyn SttClient> {
    match provider {
        "elevenlabs" => Box::new(elevenlabs::ElevenLabsClient::new(
            api_key,
            sample_rate,
            channels,
            model,
            callbacks,
        )),
        "whisper" => new_whisper_client(sample_rate, model, callbacks),
        _ => Box::new(assemblyai::AssemblyAiClient::new(
            api_key,
            sample_rate,
            model,
            callbacks,
        )),
    }
}

#[cfg(feature = "whisper")]
fn new_whisper_client(sample_rate: u32, model: &str, cbs: SttCallbacks) -> Box<dyn SttClient> {
    Box::new(whisper::WhisperClient::new(sample_rate, model, cbs))
}

#[cfg(not(feature = "whisper"))]
fn new_whisper_client(_sample_rate: u32, _model: &str, _cbs: SttCallbacks) -> Box<dyn SttClient> {
    struct WhisperStub;
    impl SttClient for WhisperStub {
        fn connect(&mut self) -> Result<()> {
            anyhow::bail!("whisper support not compiled in; rebuild with --features whisper")
        }
        fn send_audio(&self, _pcm: &[u8]) -> Result<()> {
            Ok(())
        }
        fn close(&mut self) -> Result<()> {
            Ok(())
        }
    }
    Box::new(WhisperStub)
}

/// "speaker_N" → letter 'A' + N%26; empty/unparseable → fallback.
pub fn speaker_letter(speaker_id: &str, fallback: char) -> String {
    if let Some(n) = speaker_id.strip_prefix("speaker_").and_then(|s| s.parse::<u32>().ok()) {
        char::from(b'A' + (n % 26) as u8).to_string()
    } else {
        fallback.to_string()
    }
}

/// Channel-separated recordings: channel 0 is the local mic ("You"),
/// channel N≥1 becomes Speaker A/B/...
pub fn channel_speaker(speaker_id: &str) -> String {
    match speaker_id.strip_prefix("speaker_").and_then(|s| s.parse::<u32>().ok()) {
        Some(0) => "You".to_string(),
        Some(n) => char::from(b'A' + ((n - 1) % 26) as u8).to_string(),
        None => "A".to_string(),
    }
}
