//! Batch STT adapter for ElevenLabs Scribe.
//! Audio is buffered locally; transcription runs when close() is called.

use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Result, bail};
use chrono::Local;
use serde::Deserialize;

use super::{Segment, SttCallbacks, SttClient, speaker_letter};

const STT_URL: &str = "https://api.elevenlabs.io/v1/speech-to-text";

pub struct ElevenLabsClient {
    api_key: String,
    sample_rate: u32,
    model: String,
    callbacks: SttCallbacks,
    audio_buf: Mutex<Vec<u8>>,
    closed: Mutex<bool>,
}

impl ElevenLabsClient {
    pub fn new(api_key: &str, sample_rate: u32, model: &str, callbacks: SttCallbacks) -> Self {
        ElevenLabsClient {
            api_key: api_key.to_string(),
            sample_rate,
            model: if model.is_empty() { "scribe_v1".into() } else { model.into() },
            callbacks,
            audio_buf: Mutex::new(Vec::new()),
            closed: Mutex::new(false),
        }
    }

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

impl SttClient for ElevenLabsClient {
    fn connect(&mut self) -> Result<()> {
        (self.callbacks.on_connected)();
        Ok(())
    }

    fn send_audio(&self, pcm: &[u8]) -> Result<()> {
        self.audio_buf.lock().unwrap().extend_from_slice(pcm);
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
            let mut wav = Vec::with_capacity(buf.len() + 44);
            write_wav_bytes(&mut wav, &buf, self.sample_rate);
            let segments = self.transcribe(&wav)?;
            for seg in segments {
                (self.callbacks.on_segment)(seg);
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

fn write_wav_bytes(out: &mut Vec<u8>, pcm: &[u8], sample_rate: u32) {
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
