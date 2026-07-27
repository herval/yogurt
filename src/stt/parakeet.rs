//! Local batch STT via NVIDIA Parakeet TDT and `parakeet-rs`.
//!
//! Feature-gated: `--features parakeet`. The model directory contains the
//! ONNX export of `nvidia/parakeet-tdt-0.6b-v3` downloaded by setup-parakeet.sh.

use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Result, bail};
use chrono::Local;

use super::{Segment, SttCallbacks, SttClient};

const SAMPLE_RATE: u32 = 16_000;
const CHUNK_SECONDS: usize = 240;

pub struct ParakeetClient {
    sample_rate: u32,
    model_path: PathBuf,
    callbacks: SttCallbacks,
    audio_buf: Mutex<Vec<u8>>,
    closed: Mutex<bool>,
}

impl ParakeetClient {
    pub fn new(sample_rate: u32, model: &str, callbacks: SttCallbacks) -> Self {
        Self {
            sample_rate,
            model_path: resolve_model(model),
            callbacks,
            audio_buf: Mutex::new(Vec::new()),
            closed: Mutex::new(false),
        }
    }

    fn transcribe(&self, pcm: &[u8]) -> Result<Vec<Segment>> {
        use parakeet_rs::{ParakeetTDT, Transcriber};

        let samples: Vec<f32> = pcm
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
            .collect();
        if samples.is_empty() {
            return Ok(Vec::new());
        }

        log::info!(
            "parakeet: transcribing {:.1}s of audio with {}",
            samples.len() as f64 / SAMPLE_RATE as f64,
            self.model_path.display()
        );

        let mut model = ParakeetTDT::from_pretrained(&self.model_path, None)?;
        let chunk_len = SAMPLE_RATE as usize * CHUNK_SECONDS;
        let mut segments = Vec::new();

        for (chunk_index, chunk) in samples.chunks(chunk_len).enumerate() {
            let result = model.transcribe_samples(chunk.to_vec(), SAMPLE_RATE, 1, None)?;
            let text = result.text.trim().to_string();
            if text.is_empty() {
                continue;
            }

            let start = (chunk_index * CHUNK_SECONDS) as f64;
            let end = start + chunk.len() as f64 / SAMPLE_RATE as f64;
            segments.push(Segment {
                text,
                speaker: "A".into(),
                start_time: start,
                end_time: end,
                confidence: 0.0,
                is_final: true,
                created_at: Local::now(),
            });
        }
        Ok(segments)
    }
}

impl SttClient for ParakeetClient {
    fn connect(&mut self) -> Result<()> {
        let encoder_present = [
            "encoder-model.onnx",
            "encoder.onnx",
            "encoder-model.int8.onnx",
        ]
        .iter()
        .any(|name| self.model_path.join(name).is_file());
        let decoder_present = [
            "decoder_joint-model.onnx",
            "decoder_joint-model.int8.onnx",
            "decoder_joint.onnx",
            "decoder-model.onnx",
        ]
        .iter()
        .any(|name| self.model_path.join(name).is_file());
        if !self.model_path.is_dir() || !encoder_present || !decoder_present {
            bail!(
                "parakeet model directory not found at {} — run: make setup-parakeet",
                self.model_path.display()
            );
        }
        if self.sample_rate != SAMPLE_RATE {
            bail!("parakeet requires 16kHz audio (got {}Hz)", self.sample_rate);
        }
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
            for seg in self.transcribe(&buf)? {
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

/// `path:/abs/model-dir` selects an explicit ONNX directory. Otherwise use
/// `~/.yogurt/parakeet/<model>`; the default is the v3 TDT export.
fn resolve_model(model: &str) -> PathBuf {
    if let Some(path) = model.strip_prefix("path:") {
        return PathBuf::from(path);
    }
    let name = if model.is_empty() || model == "v3" {
        "parakeet-tdt-0.6b-v3"
    } else {
        model
    };
    dirs::home_dir()
        .unwrap_or_default()
        .join(".yogurt")
        .join("parakeet")
        .join(name)
}
