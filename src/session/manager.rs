use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;

use anyhow::{Context, Result, bail};
use chrono::Local;

use crate::audio::capture::Capture;
use crate::audio::files::read_audio_file;
use crate::stt::{SttCallbacks, SttClient, new_stt_client};

use super::storage::Storage;
use super::{Session, SessionEvent, Status};

/// Streaming send sizes: AssemblyAI v3 requires 50–1000ms chunks.
/// At 16kHz PCM16: 100ms = 3200 bytes (target).
const TARGET_SEND_BYTES: usize = 3200;
const SILENT_WARN_CHUNKS: u32 = 50;

pub struct ManagerConfig {
    pub stt_provider: String,
    pub stt_api_key: String,
    pub stt_model: String,
    pub sample_rate: u32,
    pub device_index: i32,
    pub sessions_dir: PathBuf,
    /// Glossary terms biasing STT (ElevenLabs keyterms). Editable at runtime.
    pub keyterms: Vec<String>,
}

struct Active {
    session: Arc<Mutex<Session>>,
    spool_dir: PathBuf,
    capture: Capture,
    system_tap: Option<crate::audio::system_tap::SystemTap>,
    client: Arc<Mutex<Box<dyn SttClient>>>,
    audio_buf: Arc<Mutex<Vec<u8>>>,
    streamer: JoinHandle<()>,
    channels: u16,
}

pub struct Manager {
    cfg: ManagerConfig,
    device_index: AtomicI32,
    /// STT keyterms, swappable at runtime when the glossary is edited. Read at
    /// session start, so edits take effect on the next recording.
    keyterms: Mutex<Vec<String>>,
    events: mpsc::Sender<SessionEvent>,
    pub storage: Storage,
    active: Mutex<Option<Active>>,
}

impl Manager {
    pub fn new(cfg: ManagerConfig, events: mpsc::Sender<SessionEvent>) -> Arc<Manager> {
        let storage = Storage::new(cfg.sessions_dir.clone());
        let device_index = AtomicI32::new(cfg.device_index);
        let keyterms = Mutex::new(cfg.keyterms.clone());
        Arc::new(Manager {
            cfg,
            device_index,
            keyterms,
            events,
            storage,
            active: Mutex::new(None),
        })
    }

    pub fn set_device_index(&self, idx: i32) {
        self.device_index.store(idx, Ordering::SeqCst);
    }

    pub fn set_keyterms(&self, terms: Vec<String>) {
        *self.keyterms.lock().unwrap() = terms;
    }

    fn emit(&self, ev: SessionEvent) {
        let _ = self.events.send(ev);
    }

    fn make_client(
        &self,
        sample_rate: u32,
        channels: u16,
        session: &Arc<Mutex<Session>>,
        provider_name: &str,
        api_key: &str,
        model_name: &str,
    ) -> Arc<Mutex<Box<dyn SttClient>>> {
        // Segment callbacks append to the captured session directly: batch
        // providers deliver during close(), when the session may no longer be
        // "active" — this ordering bug bit the Go version.
        let sess = Arc::clone(session);
        let sess_replace = Arc::clone(session);
        let events = self.events.clone();
        let events_replace = self.events.clone();
        let events_err = self.events.clone();
        let provider = provider_name.to_string();
        let provider2 = provider.clone();
        let callbacks = SttCallbacks {
            on_segment: Arc::new(move |seg| {
                log::info!(
                    "segment: final={} speaker={} text={:?}",
                    seg.is_final,
                    seg.speaker,
                    seg.text
                );
                sess.lock().unwrap().transcript.add_segment(seg.clone());
                let _ = events.send(SessionEvent::Segment(seg));
            }),
            on_replace: Arc::new(move |segments| {
                log::info!("replacing transcript with {} authoritative segments", segments.len());
                sess_replace
                    .lock()
                    .unwrap()
                    .transcript
                    .replace_segments(segments.clone());
                let _ = events_replace.send(SessionEvent::Replace(segments));
            }),
            on_error: Arc::new(move |err| {
                log::warn!("transcription error: {err}");
                let _ = events_err.send(SessionEvent::Error(err));
            }),
            on_connected: Arc::new(move || {
                log::info!("connected to STT provider ({provider})");
            }),
            on_disconnect: Arc::new(move || {
                log::info!("disconnected from STT provider ({provider2})");
            }),
        };
        let keyterms = self.keyterms.lock().unwrap().clone();
        Arc::new(Mutex::new(new_stt_client(
            provider_name,
            api_key,
            sample_rate,
            channels,
            model_name,
            keyterms,
            callbacks,
        )))
    }

    pub fn start_session_with_stt(&self, name: &str, provider: &str, api_key: &str, model: &str) -> Result<()> {
        let mut active = self.active.lock().unwrap();
        if let Some(a) = active.as_ref() {
            if a.session.lock().unwrap().status == Status::Recording {
                bail!("already recording");
            }
        }

        let device = self.device_index.load(Ordering::SeqCst);
        log::info!(
            "starting session: device={} sampleRate={} stt={}/{}",
            device,
            self.cfg.sample_rate,
            provider, model
        );

        let mut session = Session::new(name);
        session.stt_provider = provider.to_string();
        session.stt_model = model.to_string();
        session.status = Status::Recording;
        let session = Arc::new(Mutex::new(session));

        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(512);

        // System-audio tap (remote meeting participants). With ElevenLabs the
        // streams stay on separate stereo channels (mic=L, system=R) so the
        // provider attributes speakers by channel; mono-only providers get a
        // summed mix. YOGURT_SYSTEM_AUDIO=0 disables; failure → mic-only.
        let system_audio_enabled = std::env::var("YOGURT_SYSTEM_AUDIO")
            .map(|v| v != "0")
            .unwrap_or(true);
        let stereo_capable = provider == "elevenlabs";
        let mut system_tap = None;
        let mut channels: u16 = 1;
        let mic_tx = if system_audio_enabled {
            let (sys_tx, sys_rx) = mpsc::sync_channel::<Vec<u8>>(512);
            match crate::audio::system_tap::SystemTap::start(self.cfg.sample_rate, sys_tx) {
                Ok(tap) => {
                    system_tap = Some(tap);
                    let (mic_tx, mic_rx) = mpsc::sync_channel::<Vec<u8>>(512);
                    if stereo_capable {
                        channels = 2;
                        crate::audio::mixer::spawn_interleaver(mic_rx, sys_rx, tx.clone());
                        log::info!("system audio tap active — stereo (mic=L, system=R)");
                    } else {
                        crate::audio::mixer::spawn_mixer(mic_rx, sys_rx, tx.clone());
                        log::info!("system audio tap active — mixing with mic (mono provider)");
                    }
                    mic_tx
                }
                Err(e) => {
                    log::warn!("system audio unavailable: {e:#}");
                    self.emit(SessionEvent::Notice(format!(
                        "System audio off (mic only): {e}"
                    )));
                    tx.clone()
                }
            }
        } else {
            tx.clone()
        };

        let client = self.make_client(self.cfg.sample_rate, channels, &session, provider, api_key, model);
        client
            .lock()
            .unwrap()
            .connect()
            .with_context(|| format!("connect to STT ({})", provider))?;

        let capture = Capture::start(device, self.cfg.sample_rate, mic_tx).map_err(|e| {
            let _ = client.lock().unwrap().close();
            e
        })?;

        // Crash spool: audio is appended to disk as it's captured, so a
        // crash or kill can't lose the recording (recover with --recover).
        let spool_dir = {
            let sess = session.lock().unwrap();
            self.cfg.sessions_dir.join(".recovery").join(&sess.id)
        };
        let spool_file = std::fs::create_dir_all(&spool_dir)
            .and_then(|_| {
                let meta = serde_json::json!({
                    "name": session.lock().unwrap().name,
                    "start_time": session.lock().unwrap().start_time.to_rfc3339(),
                    "sample_rate": self.cfg.sample_rate,
                    "channels": channels,
                });
                std::fs::write(spool_dir.join("meta.json"), meta.to_string())?;
                std::fs::File::create(spool_dir.join("audio.pcm"))
            })
            .map_err(|e| log::warn!("crash spool unavailable: {e}"))
            .ok();

        let audio_buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let streamer = {
            let session = Arc::clone(&session);
            let client = Arc::clone(&client);
            let audio_buf = Arc::clone(&audio_buf);
            let events = self.events.clone();
            std::thread::spawn(move || {
                stream_audio(rx, session, client, audio_buf, events, spool_file)
            })
        };

        *active = Some(Active {
            session,
            spool_dir,
            capture,
            system_tap,
            client,
            audio_buf,
            streamer,
            channels,
        });
        self.emit(SessionEvent::Status(Status::Recording));
        Ok(())
    }

    pub fn pause(&self) {
        let active = self.active.lock().unwrap();
        if let Some(a) = active.as_ref() {
            let mut sess = a.session.lock().unwrap();
            if sess.status == Status::Recording {
                sess.status = Status::Paused;
                a.capture.pause();
                drop(sess);
                self.emit(SessionEvent::Status(Status::Paused));
            }
        }
    }

    pub fn resume(&self) {
        let active = self.active.lock().unwrap();
        if let Some(a) = active.as_ref() {
            let mut sess = a.session.lock().unwrap();
            if sess.status == Status::Paused {
                sess.status = Status::Recording;
                a.capture.resume();
                drop(sess);
                self.emit(SessionEvent::Status(Status::Recording));
            }
        }
    }

    /// Blocking: stops capture, closes the STT client (batch providers deliver
    /// their segments during close), then saves. Call from a worker thread.
    pub fn finish(&self) -> Result<Option<SaveOutcome>> {
        let Some(active) = self.active.lock().unwrap().take() else {
            return Ok(None);
        };
        {
            let mut sess = active.session.lock().unwrap();
            sess.status = Status::Finished;
            sess.end_time = Some(Local::now());
        }
        self.emit(SessionEvent::Status(Status::Finished));

        // Stopping capture drops the audio sender; the mixer (if any) then the
        // streamer drain and exit.
        drop(active.capture);
        drop(active.system_tap);
        let _ = active.streamer.join();

        // Audio is saved regardless; a failed close pass must still be
        // reported so the UI can distinguish "saved" from "saved but silent".
        let stt_error = match active.client.lock().unwrap().close() {
            Ok(()) => None,
            Err(e) => {
                log::warn!("stt close: {e}");
                Some(format!("{e:#}"))
            }
        };

        let buf = std::mem::take(&mut *active.audio_buf.lock().unwrap());
        let sess = active.session.lock().unwrap();
        if buf.is_empty() && sess.transcript.segments.is_empty() {
            let _ = std::fs::remove_dir_all(&active.spool_dir);
            return Ok(None);
        }
        let folder = self.storage.save(&sess, &buf, self.cfg.sample_rate, active.channels)?;
        let _ = std::fs::remove_dir_all(&active.spool_dir);
        Ok(Some(SaveOutcome {
            folder,
            transcript: sess.transcript.to_plain_text(),
            word_count: sess.transcript.word_count(),
            stt_error,
        }))
    }

    /// Tear down without saving. Not bound in the UI (Go parity) but part of
    /// the session lifecycle API.
    #[allow(dead_code)]
    pub fn cancel(&self) {
        let Some(active) = self.active.lock().unwrap().take() else {
            return;
        };
        drop(active.capture);
        drop(active.system_tap);
        let _ = active.streamer.join();
        let _ = active.client.lock().unwrap().close();
        let _ = std::fs::remove_dir_all(&active.spool_dir);
        self.emit(SessionEvent::Status(Status::Idle));
    }

    /// UI-facing snapshot of the live session.
    pub fn snapshot(&self) -> Option<SessionSnapshot> {
        let active = self.active.lock().unwrap();
        let a = active.as_ref()?;
        let sess = a.session.lock().unwrap();
        Some(SessionSnapshot {
            duration: sess.duration_formatted(),
            word_count: sess.transcript.word_count(),
            speaker_count: sess.transcript.speakers().len(),
            plain_text: sess.transcript.to_plain_text(),
        })
    }

    /// Recover crash-spooled recordings into saved sessions.
    /// Returns (spool id, result) per spool found.
    pub fn recover_spools(&self) -> Vec<(String, Result<PathBuf>)> {
        let root = self.cfg.sessions_dir.join(".recovery");
        let Ok(entries) = std::fs::read_dir(&root) else {
            return Vec::new();
        };
        let mut results = Vec::new();
        for entry in entries.filter_map(|e| e.ok()) {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let id = entry.file_name().to_string_lossy().to_string();
            let outcome = self.recover_one(&dir);
            if outcome.is_ok() {
                let _ = std::fs::remove_dir_all(&dir);
            }
            results.push((id, outcome));
        }
        results
    }

    fn recover_one(&self, dir: &Path) -> Result<PathBuf> {
        let meta: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("meta.json"))?)?;
        let pcm = std::fs::read(dir.join("audio.pcm"))?;
        let channels = meta.get("channels").and_then(|v| v.as_u64()).unwrap_or(1) as u16;
        let sample_rate = meta
            .get("sample_rate")
            .and_then(|v| v.as_u64())
            .unwrap_or(self.cfg.sample_rate as u64) as u32;
        if pcm.len() < sample_rate as usize * 2 * channels as usize {
            bail!("less than a second of audio spooled");
        }

        let name = match meta.get("name").and_then(|v| v.as_str()).unwrap_or("") {
            "" => "recovered".to_string(),
            n => format!("{n}-recovered"),
        };
        let mut session = Session::new(&name);
        session.stt_provider = self.cfg.stt_provider.clone();
        session.stt_model = self.cfg.stt_model.clone();
        if let Some(start) = meta
            .get("start_time")
            .and_then(|v| v.as_str())
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        {
            session.start_time = start.with_timezone(&Local);
        }
        session.status = Status::Recording;
        let secs = pcm.len() as f64 / (sample_rate as f64 * 2.0 * channels as f64);
        let end_time = session.start_time + chrono::Duration::milliseconds((secs * 1000.0) as i64);
        let session = Arc::new(Mutex::new(session));

        let client = self.make_client(sample_rate, channels, &session, &self.cfg.stt_provider, &self.cfg.stt_api_key, &self.cfg.stt_model);
        client.lock().unwrap().connect()?;
        for chunk in pcm.chunks(TARGET_SEND_BYTES * channels as usize) {
            client.lock().unwrap().send_audio(chunk)?;
        }
        client.lock().unwrap().close()?;

        {
            let mut sess = session.lock().unwrap();
            sess.status = Status::Finished;
            sess.end_time = Some(end_time);
        }
        let sess = session.lock().unwrap();
        self.storage.save(&sess, &pcm, sample_rate, channels)
    }

    /// Count of crash spools awaiting recovery.
    pub fn pending_spools(&self) -> usize {
        std::fs::read_dir(self.cfg.sessions_dir.join(".recovery"))
            .map(|d| d.filter_map(|e| e.ok()).filter(|e| e.path().is_dir()).count())
            .unwrap_or(0)
    }

    /// Headless file transcription through the same STT pipeline.
    pub fn transcribe_file(
        &self,
        name: &str,
        path: &Path,
        on_progress: &mut dyn FnMut(usize, usize),
    ) -> Result<PathBuf> {
        let (pcm, file_rate) = read_audio_file(path).context("read audio file")?;

        let mut session = Session::new(name);
        session.status = Status::Recording;
        let session = Arc::new(Mutex::new(session));

        // Declare the file's real sample rate to the STT provider (the Go
        // version wrongly declared the configured mic rate).
        let client = self.make_client(file_rate, 1, &session, &self.cfg.stt_provider, &self.cfg.stt_api_key, &self.cfg.stt_model);
        client
            .lock()
            .unwrap()
            .connect()
            .with_context(|| format!("connect to STT ({})", self.cfg.stt_provider))?;

        let total = pcm.len();
        let mut sent = 0usize;
        for chunk in pcm.chunks(TARGET_SEND_BYTES) {
            client.lock().unwrap().send_audio(chunk)?;
            sent += chunk.len();
            on_progress(sent, total);
        }
        client.lock().unwrap().close()?;

        {
            let mut sess = session.lock().unwrap();
            sess.status = Status::Finished;
            sess.end_time = Some(Local::now());
        }
        let sess = session.lock().unwrap();
        self.storage.save(&sess, &pcm, file_rate, 1)
    }
}

pub struct SessionSnapshot {
    pub duration: String,
    pub word_count: usize,
    pub speaker_count: usize,
    pub plain_text: String,
}

/// Result of a successful save: where it went, the final transcript (batch
/// providers deliver segments during close, after any UI snapshot), and any
/// STT failure that left the transcript incomplete.
pub struct SaveOutcome {
    pub folder: PathBuf,
    pub transcript: String,
    pub word_count: usize,
    pub stt_error: Option<String>,
}

fn stream_audio(
    rx: mpsc::Receiver<Vec<u8>>,
    session: Arc<Mutex<Session>>,
    client: Arc<Mutex<Box<dyn SttClient>>>,
    audio_buf: Arc<Mutex<Vec<u8>>>,
    events: mpsc::Sender<SessionEvent>,
    mut spool: Option<std::fs::File>,
) {
    let mut send_buf: Vec<u8> = Vec::new();
    let mut total_chunks: u64 = 0;
    let mut silent_chunks: u32 = 0;
    let mut silent_warned = false;

    let flush = |send_buf: &mut Vec<u8>, total_chunks: &mut u64| {
        if send_buf.is_empty() {
            return;
        }
        *total_chunks += 1;
        if *total_chunks <= 5 || *total_chunks % 50 == 0 {
            log::info!("sending audio chunk #{}: {} bytes", total_chunks, send_buf.len());
        }
        if let Err(e) = client.lock().unwrap().send_audio(send_buf) {
            log::warn!("send audio error: {e}");
            let _ = events.send(SessionEvent::Notice(format!("audio send error: {e}")));
        }
        send_buf.clear();
    };

    while let Ok(chunk) = rx.recv() {
        let status = session.lock().unwrap().status;
        match status {
            Status::Paused => {
                send_buf.clear();
                let _ = events.send(SessionEvent::AudioLevel(0.0));
                continue;
            }
            Status::Recording => {}
            _ => {
                flush(&mut send_buf, &mut total_chunks);
                return;
            }
        }

        audio_buf.lock().unwrap().extend_from_slice(&chunk);
        if let Some(f) = spool.as_mut() {
            if f.write_all(&chunk).is_err() {
                log::warn!("crash spool write failed; disabling spool");
                spool = None;
            }
        }

        let level = crate::audio::calc_level(&chunk);
        if level == 0.0 {
            silent_chunks += 1;
            if silent_chunks == SILENT_WARN_CHUNKS && !silent_warned {
                silent_warned = true;
                log::warn!("no audio detected after {SILENT_WARN_CHUNKS} chunks — check mic");
                let _ = events.send(SessionEvent::Notice(
                    "No audio detected — check your microphone".into(),
                ));
            }
        } else {
            silent_chunks = 0;
        }
        let _ = events.send(SessionEvent::AudioLevel(level));

        send_buf.extend_from_slice(&chunk);
        if send_buf.len() >= TARGET_SEND_BYTES {
            flush(&mut send_buf, &mut total_chunks);
        }
    }
    flush(&mut send_buf, &mut total_chunks);
}
