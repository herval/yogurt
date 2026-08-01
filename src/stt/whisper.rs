//! Local batch STT via whisper.cpp (whisper-rs). Feature-gated: `--features whisper`.

use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Result, bail};
use chrono::Local;

use super::{Segment, SttCallbacks, SttClient};

pub struct WhisperClient {
    sample_rate: u32,
    model_path: PathBuf,
    callbacks: SttCallbacks,
    audio_buf: Mutex<Vec<u8>>,
    closed: Mutex<bool>,
}

impl WhisperClient {
    pub fn new(sample_rate: u32, model: &str, callbacks: SttCallbacks) -> Self {
        WhisperClient {
            sample_rate,
            model_path: resolve_model(model),
            callbacks,
            audio_buf: Mutex::new(Vec::new()),
            closed: Mutex::new(false),
        }
    }

    fn transcribe(&self, pcm: &[u8]) -> Result<Vec<Segment>> {
        use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

        let samples: Vec<f32> = pcm
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
            .collect();
        log::info!(
            "whisper: transcribing {:.1}s of audio with {}",
            samples.len() as f64 / self.sample_rate as f64,
            self.model_path.display()
        );

        // whisper.cpp and GGML log to stderr by default, which corrupts the
        // alt-screen TUI. Route them through `log` (i.e. into yogurt.log).
        static LOG_HOOKS: std::sync::Once = std::sync::Once::new();
        LOG_HOOKS.call_once(whisper_rs::install_logging_hooks);

        let ctx = WhisperContext::new_with_params(
            &self.model_path.to_string_lossy(),
            WhisperContextParameters::default(),
        )?;
        let mut state = ctx.create_state()?;
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });

        params.set_language(Some("auto"));
        state.full(params, &samples)?;

        let mut segments = Vec::new();
        for i in 0..state.full_n_segments()? {
            let text = state.full_get_segment_text(i)?.trim().to_string();
            if text.is_empty() {
                continue;
            }
            segments.push(Segment {
                text,
                speaker: "A".into(), // no diarization
                start_time: state.full_get_segment_t0(i)? as f64 * 0.01,
                end_time: state.full_get_segment_t1(i)? as f64 * 0.01,
                confidence: 0.0,
                is_final: true,
                created_at: Local::now(),
            });
        }
        Ok(segments)
    }
}

impl SttClient for WhisperClient {
    fn connect(&mut self) -> Result<()> {
        if !self.model_path.exists() {
            bail!(
                "whisper model not found at {} — run: make setup MODEL=<name>",
                self.model_path.display()
            );
        }
        if self.sample_rate != 16000 {
            bail!("whisper requires 16kHz audio (got {}Hz)", self.sample_rate);
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
            if buf.is_empty() {
                return Ok(());
            }
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

/// "path:/abs/model.bin" → literal path; otherwise ~/.yogurt/whisper/ggml-<model>.bin
fn resolve_model(model: &str) -> PathBuf {
    if let Some(p) = model.strip_prefix("path:") {
        return PathBuf::from(p);
    }
    let name = if model.is_empty() { "base" } else { model };
    dirs::home_dir()
        .unwrap_or_default()
        .join(".yogurt")
        .join("whisper")
        .join(format!("ggml-{name}.bin"))
}
