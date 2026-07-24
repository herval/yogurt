//! Batch STT adapter for ElevenLabs Scribe, with quasi-live windows.
//!
//! Live mode (default, every 10s — YOGURT_ELEVENLABS_LIVE_SECS to tune, 0 to
//! disable): new audio is transcribed in windows cut at silence boundaries so
//! words stay whole, and delivered as it accumulates.
//!
//! Close pass:
//! - Stereo (channel-separated) recordings: the mic channel's "You" segments
//!   from the live windows are kept; only the un-transcribed tail plus a
//!   mono diarized pass over the remote channel are uploaded. Remote speakers
//!   get whole-recording-consistent labels (A/B/C...) at ~¼ the upload cost
//!   of re-transcribing everything.
//! - Mono recordings: the whole recording is re-transcribed with diarization
//!   (per-window speaker labels can't be stitched consistently).
//!
//! Uploads are Ogg-Opus (~16x smaller than WAV) when the sample rate allows,
//! falling back to WAV otherwise.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use chrono::Local;
use serde::Deserialize;

use crate::audio::opus_enc::{encode_ogg_opus, opus_supports};
use crate::audio::wav::wav_header;

use super::{Segment, SttCallbacks, SttClient, channel_speaker, speaker_letter};

const STT_URL: &str = "https://api.elevenlabs.io/v1/speech-to-text";
const DEFAULT_LIVE_SECS: u64 = 10;
/// Don't bother transcribing live windows shorter than this.
const MIN_WINDOW_SECS: usize = 2;
/// Timeout for small live-window uploads.
const LIVE_TIMEOUT: Duration = Duration::from_secs(120);
/// Timeout for close-pass uploads (can cover a whole meeting).
const CLOSE_TIMEOUT: Duration = Duration::from_secs(900);
/// Failed live windows are rewound and retried in the next window, until the
/// backlog reaches this size — a long outage shouldn't re-upload an
/// ever-growing payload every interval.
const LIVE_RETRY_MAX_SECS: usize = 120;

pub struct ElevenLabsClient {
    api: Arc<Api>,
    sample_rate: u32,
    channels: u16,
    callbacks: SttCallbacks,
    audio_buf: Arc<Mutex<Vec<u8>>>,
    closed: Mutex<bool>,
    live: Option<LiveState>,
    /// Mic-channel segments delivered by live windows (stereo mode only);
    /// they are authoritative and reused in the close pass.
    live_you: Arc<Mutex<Vec<Segment>>>,
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
    pub fn new(
        api_key: &str,
        sample_rate: u32,
        channels: u16,
        model: &str,
        callbacks: SttCallbacks,
    ) -> Self {
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
            channels,
            callbacks,
            audio_buf: Arc::new(Mutex::new(Vec::new())),
            closed: Mutex::new(false),
            live,
            live_you: Arc::new(Mutex::new(Vec::new())),
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
            let min_bytes = MIN_WINDOW_SECS * self.sample_rate as usize * 2 * self.channels as usize;
            if buf.len().saturating_sub(start) < min_bytes {
                live.in_flight.store(false, Ordering::SeqCst);
                return;
            }
            // Cut at a silence near the end rather than mid-word: the words
            // the fixed boundary would split stay whole for the next window.
            let pending = &buf[start..];
            let cut = silence_cut_point(pending, self.sample_rate, self.channels);
            pending[..cut].to_vec()
        };
        live.consumed.store(start + window.len(), Ordering::SeqCst);

        let api = Arc::clone(&self.api);
        let callbacks = self.callbacks.clone();
        let sample_rate = self.sample_rate;
        let channels = self.channels;
        let in_flight = Arc::clone(&live.in_flight);
        let delivered = Arc::clone(&live.delivered);
        let consumed = Arc::clone(&live.consumed);
        let live_you = Arc::clone(&self.live_you);
        let offset_secs = start as f64 / (sample_rate as f64 * 2.0 * channels as f64);
        std::thread::spawn(move || {
            let payload = audio_payload(&window, sample_rate, channels);
            match api.transcribe(&payload, channels > 1, LIVE_TIMEOUT) {
                Ok(segments) => {
                    for mut seg in segments {
                        seg.start_time += offset_secs;
                        seg.end_time += offset_secs;
                        delivered.store(true, Ordering::SeqCst);
                        if channels > 1 && seg.speaker == "You" {
                            live_you.lock().unwrap().push(seg.clone());
                        }
                        (callbacks.on_segment)(seg);
                    }
                }
                Err(e) => {
                    log::warn!("live transcription window failed: {e}");
                    (callbacks.on_error)(format!("live transcription failed: {e}"));
                    // Rewind so the next window retries this audio: a
                    // transient blip then loses nothing. Safe because
                    // in_flight serializes windows, and it's released after.
                    let max_retry =
                        LIVE_RETRY_MAX_SECS * sample_rate as usize * 2 * channels as usize;
                    if window.len() <= max_retry {
                        consumed.store(start, Ordering::SeqCst);
                    }
                }
            }
            in_flight.store(false, Ordering::SeqCst);
        });
    }

    /// Stereo close pass: keep live "You" segments, transcribe the tail for
    /// the remaining "You" coverage, and diarize the remote channel over the
    /// whole recording for consistent A/B/C labels.
    fn close_multichannel(&self, buf: &[u8]) -> Result<Vec<Segment>> {
        let consumed = self
            .live
            .as_ref()
            .map(|l| l.consumed.load(Ordering::SeqCst))
            .unwrap_or(0);
        let mut you: Vec<Segment> = std::mem::take(&mut *self.live_you.lock().unwrap());

        // Tail: audio no live window covered (the whole recording when live
        // mode is off). Only its mic-channel segments are kept — the remote
        // pass below covers the other channel authoritatively.
        let tail = &buf[consumed.min(buf.len())..];
        let min_tail = self.sample_rate as usize * 2 * self.channels as usize / 2; // 0.5s
        if tail.len() >= min_tail {
            let offset = consumed as f64 / (self.sample_rate as f64 * 2.0 * self.channels as f64);
            let payload = audio_payload(tail, self.sample_rate, self.channels);
            for mut seg in self.api.transcribe(&payload, true, CLOSE_TIMEOUT)? {
                seg.start_time += offset;
                seg.end_time += offset;
                if seg.speaker == "You" {
                    you.push(seg);
                }
            }
        }

        // Remote channel, mono, diarized over the full recording.
        let remote = split_channel(buf, 1, self.channels);
        let payload = audio_payload(&remote, self.sample_rate, 1);
        let mut merged = you;
        merged.extend(self.api.transcribe(&payload, false, CLOSE_TIMEOUT)?);
        merged.sort_by(|a, b| a.start_time.total_cmp(&b.start_time));
        Ok(merged)
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
            let live_delivered = self
                .live
                .as_ref()
                .is_some_and(|l| l.delivered.load(Ordering::SeqCst));

            if self.channels > 1 {
                let merged = self.close_multichannel(&buf)?;
                if !merged.is_empty() {
                    (self.callbacks.on_replace)(merged);
                }
            } else {
                let payload = audio_payload(&buf, self.sample_rate, 1);
                let segments = self.api.transcribe(&payload, false, CLOSE_TIMEOUT)?;
                if live_delivered {
                    (self.callbacks.on_replace)(segments);
                } else {
                    for seg in segments {
                        (self.callbacks.on_segment)(seg);
                    }
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

struct AudioPayload {
    bytes: Vec<u8>,
    filename: &'static str,
    mime: &'static str,
}

/// Prefer Ogg-Opus (small); fall back to WAV for unsupported sample rates.
fn audio_payload(pcm: &[u8], sample_rate: u32, channels: u16) -> AudioPayload {
    if opus_supports(sample_rate) && channels <= 2 {
        match encode_ogg_opus(pcm, sample_rate, channels) {
            Ok(bytes) => {
                return AudioPayload {
                    bytes,
                    filename: "audio.ogg",
                    mime: "audio/ogg",
                };
            }
            Err(e) => log::warn!("opus encode failed, falling back to wav: {e}"),
        }
    }
    let mut bytes = wav_header(pcm.len() as u32, sample_rate, channels);
    bytes.extend_from_slice(pcm);
    AudioPayload {
        bytes,
        filename: "audio.wav",
        mime: "audio/wav",
    }
}

/// Extract one channel from interleaved PCM16 LE.
fn split_channel(pcm: &[u8], channel: usize, channels: u16) -> Vec<u8> {
    let ch = channels as usize;
    if ch <= 1 {
        return pcm.to_vec();
    }
    let mut out = Vec::with_capacity(pcm.len() / ch);
    for frame in pcm.chunks_exact(2 * ch) {
        let off = channel * 2;
        out.extend_from_slice(&frame[off..off + 2]);
    }
    out
}

impl Api {
    fn transcribe(
        &self,
        audio: &AudioPayload,
        multichannel: bool,
        timeout: Duration,
    ) -> Result<Vec<Segment>> {
        let boundary = "----yogurt-multipart-boundary";
        let mut body: Vec<u8> = Vec::with_capacity(audio.bytes.len() + 512);
        let mut field = |name: &str, value: &str| {
            body.extend_from_slice(
                format!(
                    "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
                )
                .as_bytes(),
            );
        };
        field("model_id", &self.model);
        if multichannel {
            field("use_multi_channel", "true");
            field("multichannel_output_style", "combined");
        } else {
            field("diarize", "true");
        }
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\nContent-Type: {}\r\n\r\n",
                audio.filename, audio.mime
            )
            .as_bytes(),
        );
        body.extend_from_slice(&audio.bytes);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
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
        Ok(to_segments(parsed, multichannel))
    }
}

/// Find a frame-aligned cut point at the quietest spot in the final seconds
/// of the pending audio, so live windows don't slice words in half. Falls
/// back to the full length when speech is continuous.
fn silence_cut_point(pcm: &[u8], sample_rate: u32, channels: u16) -> usize {
    const FRAME_MS: usize = 20;
    const SEARCH_SECS: usize = 4;
    const QUIET_SPAN_FRAMES: usize = 10; // 200ms of quiet counts as a gap
    const QUIET_PEAK: i32 = 800; // ~2.4% of full scale

    let bytes_per_frame = (sample_rate as usize / 1000) * FRAME_MS * 2 * channels as usize;
    if bytes_per_frame == 0 || pcm.len() < bytes_per_frame * QUIET_SPAN_FRAMES {
        return pcm.len();
    }
    let total_frames = pcm.len() / bytes_per_frame;
    let search_frames = (SEARCH_SECS * 1000 / FRAME_MS).min(total_frames);
    let first_frame = total_frames - search_frames;

    let frame_peak = |i: usize| -> i32 {
        let sl = &pcm[i * bytes_per_frame..(i + 1) * bytes_per_frame];
        sl.chunks_exact(2)
            .map(|c| (i16::from_le_bytes([c[0], c[1]]) as i32).abs())
            .max()
            .unwrap_or(0)
    };

    // Latest run of QUIET_SPAN_FRAMES consecutive quiet frames wins.
    let mut best_cut = None;
    let mut run = 0usize;
    for i in first_frame..total_frames {
        if frame_peak(i) < QUIET_PEAK {
            run += 1;
            if run >= QUIET_SPAN_FRAMES {
                // Cut in the middle of the quiet run.
                best_cut = Some((i + 1 - run / 2) * bytes_per_frame);
            }
        } else {
            run = 0;
        }
    }
    best_cut.unwrap_or(pcm.len())
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

/// Longest silence within one speaker's utterance before we split segments.
const MAX_UTTERANCE_GAP_SECS: f64 = 2.0;

/// Group words into per-speaker utterance segments.
///
/// Mono + diarization: words arrive sequentially, group consecutive runs.
/// Multichannel: words from different channels interleave in time, so group
/// each speaker's words separately (splitting on silence gaps) and sort the
/// resulting segments by start time.
fn to_segments(resp: ElevenLabsResponse, multichannel: bool) -> Vec<Segment> {
    let label = move |speaker_id: &str| {
        if multichannel {
            channel_speaker(speaker_id)
        } else {
            speaker_letter(speaker_id, 'A')
        }
    };
    if multichannel {
        return to_segments_by_speaker(resp, label);
    }
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
            speaker: label(&group[0].speaker_id),
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

fn to_segments_by_speaker(
    resp: ElevenLabsResponse,
    label: impl Fn(&str) -> String,
) -> Vec<Segment> {
    let words: Vec<&ElWord> = resp.words.iter().filter(|w| w.kind == "word").collect();
    if words.is_empty() {
        return Vec::new();
    }

    // Partition into per-speaker word streams, preserving order.
    let mut speakers: Vec<(&str, Vec<&ElWord>)> = Vec::new();
    for w in words {
        match speakers.iter_mut().find(|(id, _)| *id == w.speaker_id) {
            Some((_, list)) => list.push(w),
            None => speakers.push((w.speaker_id.as_str(), vec![w])),
        }
    }

    let mut segments = Vec::new();
    for (speaker_id, list) in speakers {
        let speaker = label(speaker_id);
        let mut group: Vec<&ElWord> = Vec::new();
        let flush = |group: &mut Vec<&ElWord>, segments: &mut Vec<Segment>| {
            if group.is_empty() {
                return;
            }
            let texts: Vec<&str> = group.iter().map(|w| w.text.as_str()).collect();
            segments.push(Segment {
                text: texts.join(" "),
                speaker: speaker.clone(),
                start_time: group[0].start,
                end_time: group.last().unwrap().end,
                confidence: 1.0,
                is_final: true,
                created_at: Local::now(),
            });
            group.clear();
        };
        for w in list {
            if let Some(prev) = group.last() {
                if w.start - prev.end > MAX_UTTERANCE_GAP_SECS {
                    flush(&mut group, &mut segments);
                }
            }
            group.push(w);
        }
        flush(&mut group, &mut segments);
    }

    segments.sort_by(|a, b| a.start_time.total_cmp(&b.start_time));
    segments
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(frames_ms: usize, amp: i16, rate: u32) -> Vec<u8> {
        let samples = rate as usize / 1000 * frames_ms;
        let mut out = Vec::with_capacity(samples * 2);
        for i in 0..samples {
            let v = if i % 2 == 0 { amp } else { -amp };
            out.extend_from_slice(&v.to_le_bytes());
        }
        out
    }

    #[test]
    fn cuts_at_trailing_silence() {
        let rate = 16000;
        let mut pcm = tone(9000, 8000, rate); // 9s loud speech
        let silence_start = pcm.len();
        pcm.extend(tone(500, 0, rate)); // 0.5s silence
        pcm.extend(tone(500, 8000, rate)); // 0.5s speech again
        let cut = silence_cut_point(&pcm, rate, 1);
        assert!(
            cut > silence_start && cut < silence_start + rate as usize,
            "cut {cut} not inside silence at {silence_start}"
        );
        assert_eq!(cut % 2, 0);
    }

    #[test]
    fn continuous_speech_keeps_everything() {
        let rate = 16000;
        let pcm = tone(10000, 8000, rate);
        assert_eq!(silence_cut_point(&pcm, rate, 1), pcm.len());
    }

    #[test]
    fn split_channel_extracts_interleaved() {
        // Frames: L=1, R=2 repeated
        let mut pcm = Vec::new();
        for _ in 0..4 {
            pcm.extend_from_slice(&1i16.to_le_bytes());
            pcm.extend_from_slice(&2i16.to_le_bytes());
        }
        let left = split_channel(&pcm, 0, 2);
        let right = split_channel(&pcm, 1, 2);
        assert!(left.chunks_exact(2).all(|c| i16::from_le_bytes([c[0], c[1]]) == 1));
        assert!(right.chunks_exact(2).all(|c| i16::from_le_bytes([c[0], c[1]]) == 2));
        assert_eq!(left.len(), pcm.len() / 2);
    }

    #[test]
    fn payload_prefers_opus_and_falls_back_to_wav() {
        let pcm = tone(1000, 4000, 16000);
        let p = audio_payload(&pcm, 16000, 1);
        assert_eq!(p.filename, "audio.ogg");
        assert!(p.bytes.len() < pcm.len() / 4);

        let p = audio_payload(&pcm, 44100, 1);
        assert_eq!(p.filename, "audio.wav");
        assert_eq!(&p.bytes[0..4], b"RIFF");
    }
}
