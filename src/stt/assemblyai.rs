//! Streaming STT via AssemblyAI's v3 realtime websocket.
//!
//! One thread owns the socket (blocking tungstenite can't be split): it drains
//! an outbound command queue, then reads with a short timeout, forever.

use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Local;
use tungstenite::client::IntoClientRequest;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

use super::{Segment, SttCallbacks, SttClient};

const WS_URL: &str = "wss://streaming.assemblyai.com/v3/ws";

enum Cmd {
    Audio(Vec<u8>),
    Terminate,
}

pub struct AssemblyAiClient {
    api_key: String,
    sample_rate: u32,
    model: String,
    callbacks: SttCallbacks,
    outbound: Mutex<Option<mpsc::Sender<Cmd>>>,
    connected: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl AssemblyAiClient {
    pub fn new(api_key: &str, sample_rate: u32, model: &str, callbacks: SttCallbacks) -> Self {
        AssemblyAiClient {
            api_key: api_key.to_string(),
            sample_rate,
            model: model.to_string(),
            callbacks,
            outbound: Mutex::new(None),
            connected: Arc::new(AtomicBool::new(false)),
            worker: None,
        }
    }
}

impl SttClient for AssemblyAiClient {
    fn connect(&mut self) -> Result<()> {
        let url = format!(
            "{WS_URL}?sample_rate={}&encoding=pcm_s16le&speech_model={}&diarization=true",
            self.sample_rate, self.model
        );
        let mut request = url.into_client_request().context("build ws request")?;
        request.headers_mut().insert(
            "Authorization",
            self.api_key.parse().context("api key header")?,
        );

        let (mut socket, _resp) =
            tungstenite::connect(request).context("connect to assemblyai")?;
        set_read_timeout(&mut socket, Duration::from_millis(50));

        let (tx, rx) = mpsc::channel::<Cmd>();
        *self.outbound.lock().unwrap() = Some(tx);
        self.connected.store(true, Ordering::SeqCst);

        let callbacks = self.callbacks.clone();
        let connected = Arc::clone(&self.connected);
        self.worker = Some(std::thread::spawn(move || {
            ws_loop(socket, rx, callbacks, connected);
        }));

        (self.callbacks.on_connected)();
        Ok(())
    }

    fn send_audio(&self, pcm: &[u8]) -> Result<()> {
        if !self.connected.load(Ordering::SeqCst) {
            return Ok(()); // silently no-op, Go parity
        }
        if let Some(tx) = self.outbound.lock().unwrap().as_ref() {
            let _ = tx.send(Cmd::Audio(pcm.to_vec()));
        }
        Ok(())
    }

    fn close(&mut self) -> Result<()> {
        if let Some(tx) = self.outbound.lock().unwrap().take() {
            let _ = tx.send(Cmd::Terminate);
            // Dropping tx after Terminate lets the ws thread finish its dance.
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.connected.store(false, Ordering::SeqCst);
        (self.callbacks.on_disconnect)();
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }
}

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

fn set_read_timeout(socket: &mut Socket, dur: Duration) {
    let stream = match socket.get_mut() {
        MaybeTlsStream::Plain(s) => s,
        MaybeTlsStream::Rustls(s) => s.get_mut(),
        _ => return,
    };
    let _ = stream.set_read_timeout(Some(dur));
}

fn ws_loop(
    mut socket: Socket,
    rx: mpsc::Receiver<Cmd>,
    callbacks: SttCallbacks,
    connected: Arc<AtomicBool>,
) {
    let mut terminating = false;
    loop {
        // Drain outbound commands.
        loop {
            match rx.try_recv() {
                Ok(Cmd::Audio(pcm)) => {
                    if let Err(e) = socket.send(Message::Binary(pcm.into())) {
                        log::warn!("assemblyai send: {e}");
                    }
                }
                Ok(Cmd::Terminate) => {
                    let _ = socket.send(Message::Text(r#"{"type":"Terminate"}"#.into()));
                    std::thread::sleep(Duration::from_millis(300));
                    let _ = socket.close(None);
                    terminating = true;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    if !terminating {
                        let _ = socket.close(None);
                        terminating = true;
                    }
                    break;
                }
            }
        }

        // Read with the socket's short timeout.
        match socket.read() {
            Ok(Message::Text(text)) => handle_message(&text, &callbacks),
            Ok(Message::Close(_)) => break,
            Ok(_) => {}
            Err(tungstenite::Error::Io(e))
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                if terminating {
                    break;
                }
            }
            Err(tungstenite::Error::ConnectionClosed) | Err(tungstenite::Error::AlreadyClosed) => {
                break;
            }
            Err(e) => {
                if !terminating {
                    (callbacks.on_error)(format!("assemblyai: {e}"));
                }
                break;
            }
        }
    }
    connected.store(false, Ordering::SeqCst);
}

fn handle_message(text: &str, callbacks: &SttCallbacks) {
    let Ok(msg) = serde_json::from_str::<serde_json::Value>(text) else {
        return;
    };
    match msg.get("type").and_then(|t| t.as_str()) {
        Some("Turn") => handle_turn(&msg, callbacks),
        Some("Error") => {
            let detail = msg
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("unknown error");
            (callbacks.on_error)(format!("assemblyai: {detail}"));
        }
        _ => {} // Begin, Termination: no-op
    }
}

fn handle_turn(msg: &serde_json::Value, callbacks: &SttCallbacks) {
    let transcript = msg
        .get("transcript")
        .and_then(|t| t.as_str())
        .unwrap_or("");
    if transcript.is_empty() {
        return;
    }
    let turn_order = msg.get("turn_order").and_then(|v| v.as_u64()).unwrap_or(0);
    let is_final = msg
        .get("end_of_turn")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let mut start_time = 0.0;
    let mut end_time = 0.0;
    let mut confidence = 1.0;
    let mut speaker = String::new();

    if let Some(words) = msg.get("words").and_then(|w| w.as_array()) {
        if !words.is_empty() {
            // Word times come in milliseconds.
            start_time = words[0].get("start").and_then(|v| v.as_f64()).unwrap_or(0.0) / 1000.0;
            end_time = words
                .last()
                .unwrap()
                .get("end")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0)
                / 1000.0;
            let confs: Vec<f64> = words
                .iter()
                .map(|w| w.get("confidence").and_then(|v| v.as_f64()).unwrap_or(1.0))
                .collect();
            confidence = confs.iter().sum::<f64>() / confs.len() as f64;
            if let Some(sp) = words[0].get("speaker").and_then(|v| v.as_str()) {
                if let Some(n) = sp.strip_prefix("speaker_").and_then(|s| s.parse::<u32>().ok()) {
                    speaker = char::from(b'A' + (n % 26) as u8).to_string();
                }
            }
        }
    }
    if speaker.is_empty() {
        speaker = char::from(b'A' + (turn_order % 26) as u8).to_string();
    }

    (callbacks.on_segment)(Segment {
        text: transcript.to_string(),
        speaker,
        start_time,
        end_time,
        confidence,
        is_final,
        created_at: Local::now(),
    });
}
