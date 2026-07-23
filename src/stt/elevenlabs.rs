//! Batch STT adapter for ElevenLabs Scribe, with optional quasi-live mode.
//!
//! Base behavior: audio is buffered locally and transcribed once on close().
//! Quasi-live (default every 10s, tune/disable via YOGURT_ELEVENLABS_LIVE_SECS):
//! while recording, each new window of audio is transcribed and delivered as
//! it accumulates, then close() re-transcribes the WHOLE recording and
//! replaces the live segments (consistent diarization for the saved session).
//! Speaker letters may shift between the live view and the final pass.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use chrono::Local;
use serde::Deserialize;

use super::{Segment, SttCallbacks, SttClient, speaker_letter};

const STT_URL: &str = "https://api.elevenlabs.io/v1/speech-to-text";
const DEFAULT_LIVE_SECS: u64 = 10;
/// Don't bother transcribing live windows shorter than this.
const MIN_WINDOW_SECS: usize = 2;

pub struct ElevenLabsClient {
    api: Arc<Api>,
    sample_rate: u32,
    callbacks: SttCallbacks,
    audio_buf: Arc<Mutex<Vec<u8>>>,
    closed: Mutex<bool>,
    live: Option<LiveState>,
}

/// Request parameters shared with live-window worker threads.
struct Api {
    api_key: String,
    model: String,
}

struct LiveState {
    interval: Duration,
    last_flush: Mutex<Instant>,
    /// Byte offset into audio_buf that live transcription has consumed.
    consumed: Arc<AtomicUsize>,
    in_flight: Arc<AtomicBool>,
    /// True once any live window was delivered (close() must then replace).
    delivered: Arc<AtomicBool>,
}

impl ElevenLabsClient {
    pub fn new(api_key: &str, sample_rate: u32, model: &str, callbacks: SttCallbacks) -> Self {
        let live_secs = std::env::var("YOGURT_ELEVENLABS_LIVE_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(DEFAULT_LIVE_SECS);
        let live = (live_secs > 0).then(|| LiveState {
            interval: Duration::from_secs(live_secs),
            last_flush: Mutex::new(Instant::now()),
            consumed: Arc::new(AtomicUsize::new(0)),
            in_flight: Arc::new(AtomicBool::new(false)),
            delivered: Arc::new(AtomicBool::new(false)),
        });
        ElevenLabsClient {
            api: Arc::new(Api {
                api_key: api_key.to_string(),
                model: if model.is_empty() { "scribe_v1".into() } else { model.into() },
            }),
            sample_rate,
            callbacks,
            audio_buf: Arc::new(Mutex::new(Vec::new())),
            closed: Mutex::new(false),
            live,
        }
    }

    /// Kick a background transcription of the audio accumulated since the
    /// last window, if the interval elapsed and no request is in flight.
    fn maybe_flush_live(&self) {
        let Some(live) = &self.live else { return };

        {
            let mut last = live.last_flush.lock().unwrap();
            if last.elapsed() < live.interval {
                return;
            }
            *last = Instant::now();
        }
        if live.in_flight.swap(true, Ordering::SeqCst) {
            return; // previous window still transcribing
        }

        let start = live.consumed.load(Ordering::SeqCst);
        let window: Vec<u8> = {
            let buf = self.audio_buf.lock().unwrap();
            if buf.len().saturating_sub(start) < MIN_WINDOW_SECS * self.sample_rate as usize * 2 {
                live.in_flight.store(false, Ordering::SeqCst);
                return;
            }
            buf[start..].to_vec()
        };
        live.consumed.store(start + window.len(), Ordering::SeqCst);

        let api = Arc::clone(&self.api);
        let callbacks = self.callbacks.clone();
        let sample_rate = self.sample_rate;
        let in_flight = Arc::clone(&live.in_flight);
        let delivered = Arc::clone(&live.delivered);
        let offset_secs = start as f64 / (sample_rate as f64 * 2.0);
        std::thread::spawn(move || {
            let wav = wav_bytes(&window, sample_rate);
            match api.transcribe(&wav) {
                Ok(segments) => {
                    for mut seg in segments {
                        seg.start_time += offset_secs;
                        seg.end_time += offset_secs;
                        delivered.store(true, Ordering::SeqCst);
                        (callbacks.on_segment)(seg);
                    }
                }
                Err(e) => {
                    // Live windows are best-effort; the close() pass is
                    // authoritative. Log, don't scare the UI.
                    log::warn!("live transcription window failed: {e}");
                }
            }
            in_flight.store(false, Ordering::SeqCst);
        });
    }
}

impl SttClient for ElevenLabsClient {
    fn connect(&mut self) -> Result<()> {
        if let Some(live) = &self.live {
            *live.last_flush.lock().unwrap() = Instant::now();
        }
        (self.callbacks.on_connected)();
        Ok(())
    }

    fn send_audio(&self, pcm: &[u8]) -> Result<()> {
        self.audio_buf.lock().unwrap().extend_from_slice(pcm);
        self.maybe_flush_live();
        Ok(())
    }

    fn close(&mut self) -> Result<()> {
        {
            let mut closed = self.closed.lock().unwrap();
            if *closed {
                return Ok(());
            }
            *closed = true;
        }
        let buf = std::mem::take(&mut *self.audio_buf.lock().unwrap());

        let result = (|| -> Result<()> {
            if buf.is_empty() {
                return Ok(());
            }
            let wav = wav_bytes(&buf, self.sample_rate);
            let segments = self.api.transcribe(&wav)?;
            let live_delivered = self
                .live
                .as_ref()
                .is_some_and(|l| l.delivered.load(Ordering::SeqCst));
            if live_delivered {
                // Authoritative full pass supersedes the live windows.
                (self.callbacks.on_replace)(segments);
            } else {
                for seg in segments {
                    (self.callbacks.on_segment)(seg);
                }
            }
            Ok(())
        })();

        if let Err(e) = &result {
            (self.callbacks.on_error)(e.to_string());
        }
        (self.callbacks.on_disconnect)();
        result
    }
}

impl Api {
    fn transcribe(&self, wav: &[u8]) -> Result<Vec<Segment>> {
        let boundary = "----yogurt-multipart-boundary";
        let mut body: Vec<u8> = Vec::with_capacity(wav.len() + 512);
        let mut field = |name: &str, value: &str| {
            body.extend_from_slice(
                format!(
                    "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
                )
                .as_bytes(),
            );
        };
        field("model_id", &self.model);
        field("diarize", "true");
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"audio.wav\"\r\nContent-Type: audio/wav\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(wav);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(120)))
            .http_status_as_error(false)
            .build()
            .into();
        let mut resp = agent
            .post(STT_URL)
            .header("xi-api-key", &self.api_key)
            .header(
                "Content-Type",
                &format!("multipart/form-data; boundary={boundary}"),
            )
            .send(&body[..])?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string()?;
        if status != 200 {
            bail!("elevenlabs API error {status}: {text}");
        }
        let parsed: ElevenLabsResponse = serde_json::from_str(&text)
            .map_err(|e| anyhow::anyhow!("parse elevenlabs response: {e}"))?;
        Ok(to_segments(parsed))
    }
}

fn wav_bytes(pcm: &[u8], sample_rate: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(pcm.len() + 44);
    let data_len = pcm.len() as u32;
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(pcm);
    out
}

#[derive(Deserialize)]
struct ElevenLabsResponse {
    #[serde(default)]
    text: String,
    #[serde(default)]
    words: Vec<ElWord>,
}

#[derive(Deserialize)]
struct ElWord {
    #[serde(default)]
    text: String,
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    start: f64,
    #[serde(default)]
    end: f64,
    #[serde(default)]
    speaker_id: String,
}

/// Group consecutive words by speaker into segments.
fn to_segments(resp: ElevenLabsResponse) -> Vec<Segment> {
    let words: Vec<&ElWord> = resp.words.iter().filter(|w| w.kind == "word").collect();
    if words.is_empty() {
        if resp.text.is_empty() {
            return Vec::new();
        }
        return vec![Segment {
            text: resp.text,
            speaker: "A".into(),
            start_time: 0.0,
            end_time: 0.0,
            confidence: 1.0,
            is_final: true,
            created_at: Local::now(),
        }];
    }

    let mut segments = Vec::new();
    let mut group: Vec<&ElWord> = Vec::new();
    let flush = |group: &mut Vec<&ElWord>, segments: &mut Vec<Segment>| {
        if group.is_empty() {
            return;
        }
        let texts: Vec<&str> = group.iter().map(|w| w.text.as_str()).collect();
        segments.push(Segment {
            text: texts.join(" "),
            speaker: speaker_letter(&group[0].speaker_id, 'A'),
            start_time: group[0].start,
            end_time: group.last().unwrap().end,
            confidence: 1.0,
            is_final: true,
            created_at: Local::now(),
        });
        group.clear();
    };

    for w in words {
        if let Some(prev) = group.last() {
            if prev.speaker_id != w.speaker_id {
                flush(&mut group, &mut segments);
            }
        }
        group.push(w);
    }
    flush(&mut group, &mut segments);
    segments
}
