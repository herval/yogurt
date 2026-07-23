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
}

struct Active {
    session: Arc<Mutex<Session>>,
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
    events: mpsc::Sender<SessionEvent>,
    pub storage: Storage,
    active: Mutex<Option<Active>>,
}

impl Manager {
    pub fn new(cfg: ManagerConfig, events: mpsc::Sender<SessionEvent>) -> Arc<Manager> {
        let storage = Storage::new(cfg.sessions_dir.clone());
        let device_index = AtomicI32::new(cfg.device_index);
        Arc::new(Manager {
            cfg,
            device_index,
            events,
            storage,
            active: Mutex::new(None),
        })
    }

    pub fn set_device_index(&self, idx: i32) {
        self.device_index.store(idx, Ordering::SeqCst);
    }

    fn emit(&self, ev: SessionEvent) {
        let _ = self.events.send(ev);
    }

    fn make_client(
        &self,
        sample_rate: u32,
        channels: u16,
        session: &Arc<Mutex<Session>>,
    ) -> Arc<Mutex<Box<dyn SttClient>>> {
        // Segment callbacks append to the captured session directly: batch
        // providers deliver during close(), when the session may no longer be
        // "active" — this ordering bug bit the Go version.
        let sess = Arc::clone(session);
        let sess_replace = Arc::clone(session);
        let events = self.events.clone();
        let events_replace = self.events.clone();
        let events_err = self.events.clone();
        let provider = self.cfg.stt_provider.clone();
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
        Arc::new(Mutex::new(new_stt_client(
            &self.cfg.stt_provider,
            &self.cfg.stt_api_key,
            sample_rate,
            channels,
            &self.cfg.stt_model,
            callbacks,
        )))
    }

    pub fn start_session(&self, name: &str) -> Result<()> {
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
            self.cfg.stt_provider,
            self.cfg.stt_model
        );

        let mut session = Session::new(name);
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
        let stereo_capable = self.cfg.stt_provider == "elevenlabs";
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

        let client = self.make_client(self.cfg.sample_rate, channels, &session);
        client
            .lock()
            .unwrap()
            .connect()
            .with_context(|| format!("connect to STT ({})", self.cfg.stt_provider))?;

        let capture = Capture::start(device, self.cfg.sample_rate, mic_tx).map_err(|e| {
            let _ = client.lock().unwrap().close();
            e
        })?;

        let audio_buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let streamer = {
            let session = Arc::clone(&session);
            let client = Arc::clone(&client);
            let audio_buf = Arc::clone(&audio_buf);
            let events = self.events.clone();
            std::thread::spawn(move || stream_audio(rx, session, client, audio_buf, events))
        };

        *active = Some(Active {
            session,
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
    pub fn finish(&self) -> Result<Option<PathBuf>> {
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

        if let Err(e) = active.client.lock().unwrap().close() {
            log::warn!("stt close: {e}");
        }

        let buf = std::mem::take(&mut *active.audio_buf.lock().unwrap());
        let sess = active.session.lock().unwrap();
        if buf.is_empty() && sess.transcript.segments.is_empty() {
            return Ok(None);
        }
        let folder = self.storage.save(&sess, &buf, self.cfg.sample_rate, active.channels)?;
        Ok(Some(folder))
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
        let client = self.make_client(file_rate, 1, &session);
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

fn stream_audio(
    rx: mpsc::Receiver<Vec<u8>>,
    session: Arc<Mutex<Session>>,
    client: Arc<Mutex<Box<dyn SttClient>>>,
    audio_buf: Arc<Mutex<Vec<u8>>>,
    events: mpsc::Sender<SessionEvent>,
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
