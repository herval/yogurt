use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Sender;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tui_textarea::TextArea;

use crate::audio::Device;
use crate::config::Config;
use crate::llm::client::{ChatMessage, LlmClient};
use crate::llm::templates::Template;
use crate::session::manager::Manager;
use crate::session::{SessionEvent, SessionSummary, Status};
use crate::stt::Segment;

use super::msg::AppMsg;

const CHAT_CHAR_LIMIT: usize = 500;

pub struct TranscriptLine {
    pub seg: Segment,
    pub partial: bool,
}

pub struct App {
    pub mgr: Arc<Manager>,
    pub devices: Vec<Device>,
    pub cfg: Config,
    tx: Sender<AppMsg>,

    // Recording / transcript
    pub status: Status,
    pub audio_level: f64,
    pub duration: String,
    pub lines: Vec<TranscriptLine>,
    partial_idx: Option<usize>,
    pub scroll: usize,

    // Notices
    pub notice: String,
    pub notice_err: bool,

    // Mic select
    pub selecting_mic: bool,
    pub mic_idx: usize,

    // Home / sessions
    pub home_mode: bool,
    pub sessions: Vec<SessionSummary>,
    pub session_idx: usize,
    pub viewing: Option<SessionSummary>,
    pub view_raw: String,

    // Chat
    pub chat_open: bool,
    pub chat_input: TextArea<'static>,
    pub chat_msgs: Vec<ChatMessage>,
    pub chat_scroll: usize,
    pub chat_loading: bool,

    // Template picker
    pub templates: Vec<Template>,
    pub template_open: bool,
    pub template_idx: usize,

    pub confirm_delete: bool,

    quitting: bool,
    pub should_quit: bool,
}

impl App {
    pub fn new(
        mgr: Arc<Manager>,
        devices: Vec<Device>,
        cfg: Config,
        templates: Vec<Template>,
        tx: Sender<AppMsg>,
    ) -> App {
        let mut chat_input = TextArea::default();
        chat_input.set_placeholder_text("Ask about the transcript...");
        chat_input.set_cursor_line_style(ratatui::style::Style::default());
        App {
            mgr,
            devices,
            cfg,
            tx,
            status: Status::Idle,
            audio_level: 0.0,
            duration: "00:00:00".into(),
            lines: Vec::new(),
            partial_idx: None,
            scroll: 0,
            notice: String::new(),
            notice_err: false,
            selecting_mic: false,
            mic_idx: 0,
            home_mode: true,
            sessions: Vec::new(),
            session_idx: 0,
            viewing: None,
            view_raw: String::new(),
            chat_open: false,
            chat_input,
            chat_msgs: Vec::new(),
            chat_scroll: 0,
            chat_loading: false,
            templates,
            template_open: false,
            template_idx: 0,
            confirm_delete: false,
            quitting: false,
            should_quit: false,
        }
    }

    fn set_notice(&mut self, text: impl Into<String>, is_err: bool) {
        self.notice = text.into();
        self.notice_err = is_err;
    }

    pub fn update(&mut self, msg: AppMsg) {
        match msg {
            AppMsg::Key(key) => self.handle_key(key),
            AppMsg::Resize => {}
            AppMsg::Tick => {
                if let Some(snap) = self.mgr.snapshot() {
                    self.duration = snap.duration;
                }
            }
            AppMsg::Session(ev) => self.handle_session_event(ev),
            AppMsg::SaveResult {
                folder,
                transcript,
                err,
            } => {
                self.home_mode = true;
                if let Some(e) = err {
                    self.set_notice(format!("Error saving: {e}"), true);
                } else if let Some(folder) = folder {
                    if self.quitting {
                        self.should_quit = true;
                        return;
                    }
                    self.set_notice("Saved — generating title & summary...", false);
                    self.cmd_generate_meta(folder, transcript);
                } else {
                    self.set_notice("Session ended (nothing to save)", false);
                }
                if self.quitting {
                    self.should_quit = true;
                }
                self.cmd_load_sessions();
            }
            AppMsg::MetaGenerated { title, err } => {
                match err {
                    Some(e) => self.set_notice(format!("Saved (could not generate title: {e})"), false),
                    None => self.set_notice(format!("\u{201c}{title}\u{201d}"), false),
                }
                self.cmd_load_sessions();
            }
            AppMsg::ChatResponse { content, err } => {
                self.chat_loading = false;
                let content = match err {
                    Some(e) => format!("Error: {e}"),
                    None => content,
                };
                self.chat_msgs.push(ChatMessage {
                    role: "assistant".into(),
                    content,
                });
                // Persist chats on stored sessions only.
                if let Some(viewing) = &self.viewing {
                    let _ = self
                        .mgr
                        .storage
                        .save_chat(std::path::Path::new(&viewing.folder), &self.chat_msgs);
                }
            }
            AppMsg::SessionsLoaded(sessions) => {
                self.sessions = sessions;
                if self.session_idx >= self.sessions.len() {
                    self.session_idx = self.sessions.len().saturating_sub(1);
                }
            }
        }
    }

    fn handle_session_event(&mut self, ev: SessionEvent) {
        match ev {
            SessionEvent::Segment(seg) => {
                if seg.is_final {
                    if let Some(idx) = self.partial_idx.take() {
                        self.lines[idx] = TranscriptLine { seg, partial: false };
                    } else {
                        self.lines.push(TranscriptLine { seg, partial: false });
                    }
                } else if let Some(idx) = self.partial_idx {
                    self.lines[idx] = TranscriptLine { seg, partial: true };
                } else {
                    self.lines.push(TranscriptLine { seg, partial: true });
                    self.partial_idx = Some(self.lines.len() - 1);
                }
            }
            SessionEvent::Replace(segments) => {
                self.partial_idx = None;
                self.lines = segments
                    .into_iter()
                    .map(|seg| TranscriptLine { seg, partial: false })
                    .collect();
            }
            SessionEvent::Status(s) => {
                self.status = s;
                match s {
                    Status::Idle | Status::Finished => {
                        self.duration = "00:00:00".into();
                        self.audio_level = 0.0;
                    }
                    Status::Recording => self.home_mode = false,
                    _ => {}
                }
            }
            SessionEvent::Error(e) => self.set_notice(e, true),
            SessionEvent::AudioLevel(l) => self.audio_level = l,
            SessionEvent::Notice(n) => self.set_notice(n, false),
        }
    }

    // --- Key handling (modal precedence mirrors the Go version) ---

    fn handle_key(&mut self, key: KeyEvent) {
        if self.confirm_delete {
            self.handle_confirm_delete_key(key);
        } else if self.template_open {
            self.handle_template_key(key);
        } else if self.chat_open {
            self.handle_chat_key(key);
        } else {
            self.handle_base_key(key);
        }
    }

    fn handle_confirm_delete_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                if let Some(s) = self.sessions.get(self.session_idx) {
                    let _ = std::fs::remove_dir_all(&s.folder);
                    if self.session_idx > 0 {
                        self.session_idx -= 1;
                    }
                    self.cmd_load_sessions();
                }
                self.confirm_delete = false;
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.confirm_delete = false;
            }
            _ => {}
        }
    }

    fn handle_template_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.template_open = false,
            KeyCode::Up | KeyCode::Char('k') => {
                self.template_idx = self.template_idx.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.template_idx + 1 < self.templates.len() {
                    self.template_idx += 1;
                }
            }
            KeyCode::Enter => {
                self.template_open = false;
                if let Some(t) = self.templates.get(self.template_idx).cloned() {
                    // Echo the template NAME into history; send the PROMPT.
                    self.chat_msgs.push(ChatMessage {
                        role: "user".into(),
                        content: t.name.clone(),
                    });
                    self.chat_loading = true;
                    self.chat_scroll = 0;
                    self.cmd_ask(t.prompt);
                }
            }
            _ => {}
        }
    }

    fn handle_chat_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.chat_open = false;
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.chat_open = false;
            }
            KeyCode::Char('?') if self.chat_input_text().is_empty() && !self.templates.is_empty() => {
                self.template_open = true;
                self.template_idx = 0;
            }
            KeyCode::Enter => {
                let text = self.chat_input_text();
                if !text.is_empty() && !self.chat_loading {
                    self.chat_msgs.push(ChatMessage {
                        role: "user".into(),
                        content: text.clone(),
                    });
                    self.chat_input = {
                        let mut ta = TextArea::default();
                        ta.set_placeholder_text("Ask about the transcript...");
                        ta.set_cursor_line_style(ratatui::style::Style::default());
                        ta
                    };
                    self.chat_loading = true;
                    self.chat_scroll = 0;
                    self.cmd_ask(text);
                }
            }
            KeyCode::Up => self.chat_scroll += 1,
            KeyCode::Down => self.chat_scroll = self.chat_scroll.saturating_sub(1),
            _ => {
                if self.chat_input_text().len() < CHAT_CHAR_LIMIT
                    || matches!(key.code, KeyCode::Backspace | KeyCode::Delete | KeyCode::Left | KeyCode::Right)
                {
                    self.chat_input.input(key);
                }
            }
        }
    }

    pub fn chat_input_text(&self) -> String {
        self.chat_input.lines().join(" ").trim().to_string()
    }

    fn handle_base_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.quit();
            return;
        }
        if self.selecting_mic {
            self.handle_mic_key(key);
            return;
        }
        if self.home_mode {
            self.handle_home_key(key);
            return;
        }
        self.handle_live_key(key);
    }

    fn handle_mic_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.mic_idx = self.mic_idx.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                if self.mic_idx + 1 < self.devices.len() {
                    self.mic_idx += 1;
                }
            }
            KeyCode::Enter => {
                if let Some(d) = self.devices.get(self.mic_idx) {
                    self.mgr.set_device_index(d.index);
                    let name = d.name.clone();
                    self.set_notice(format!("Microphone: {name}"), false);
                }
                self.selecting_mic = false;
            }
            KeyCode::Esc | KeyCode::Char('q') => self.selecting_mic = false,
            _ => {}
        }
    }

    fn handle_home_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                if self.viewing.is_some() {
                    self.scroll += 1;
                } else {
                    self.session_idx = self.session_idx.saturating_sub(1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.viewing.is_some() {
                    self.scroll = self.scroll.saturating_sub(1);
                } else if self.session_idx + 1 < self.sessions.len() {
                    self.session_idx += 1;
                }
            }
            KeyCode::Enter => {
                if self.viewing.is_none() {
                    if let Some(s) = self.sessions.get(self.session_idx).cloned() {
                        self.view_raw = self
                            .mgr
                            .storage
                            .load_transcript(std::path::Path::new(&s.folder));
                        self.viewing = Some(s);
                        self.scroll = 0;
                        self.chat_msgs.clear();
                    }
                }
            }
            KeyCode::Esc => {
                self.viewing = None;
                self.chat_open = false;
            }
            KeyCode::Char('c') | KeyCode::Char('C') => {
                if self.viewing.is_some() {
                    self.open_chat();
                }
            }
            KeyCode::Char('n') | KeyCode::Char('N') => self.new_session(),
            KeyCode::Char('d') | KeyCode::Char('D') => {
                if self.viewing.is_none() && !self.sessions.is_empty() {
                    self.confirm_delete = true;
                }
            }
            KeyCode::Char('q') | KeyCode::Char('Q') => self.quit(),
            _ => {}
        }
    }

    fn handle_live_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('n') | KeyCode::Char('N') => {
                if matches!(self.status, Status::Idle | Status::Finished) {
                    self.new_session();
                }
            }
            KeyCode::Char('p') | KeyCode::Char('P') => match self.status {
                Status::Recording => self.mgr.pause(),
                Status::Paused => self.mgr.resume(),
                _ => {}
            },
            KeyCode::Char('f') | KeyCode::Char('F') => {
                if matches!(self.status, Status::Recording | Status::Paused) {
                    self.cmd_finish();
                }
            }
            KeyCode::Char('m') | KeyCode::Char('M') => {
                if self.status != Status::Recording {
                    self.selecting_mic = true;
                    self.mic_idx = 0;
                }
            }
            KeyCode::Char('c') | KeyCode::Char('C') => self.open_chat(),
            KeyCode::Char('q') | KeyCode::Char('Q') => self.quit(),
            KeyCode::Up => self.scroll += 1,
            KeyCode::Down => self.scroll = self.scroll.saturating_sub(1),
            _ => {}
        }
    }

    fn open_chat(&mut self) {
        if self.cfg.llm_api_key.is_empty() {
            let provider = self.cfg.llm_provider.to_uppercase();
            self.set_notice(
                format!("Set LLM_MODEL and {provider}_API_KEY to enable chat"),
                true,
            );
            return;
        }
        self.chat_open = true;
        self.chat_scroll = 0;
    }

    fn new_session(&mut self) {
        self.lines.clear();
        self.partial_idx = None;
        self.scroll = 0;
        self.duration = "00:00:00".into();
        self.notice.clear();
        self.home_mode = false;
        self.viewing = None;
        self.chat_open = false;
        self.cmd_start_session();
    }

    fn quit(&mut self) {
        if matches!(self.status, Status::Recording | Status::Paused) {
            self.quitting = true;
            self.cmd_finish();
        } else {
            self.should_quit = true;
        }
    }

    // --- Worker commands (tea.Cmd analogs) ---

    fn cmd_load_sessions(&self) {
        let tx = self.tx.clone();
        let mgr = Arc::clone(&self.mgr);
        std::thread::spawn(move || {
            let sessions = mgr.storage.list_sessions();
            let _ = tx.send(AppMsg::SessionsLoaded(sessions));
        });
    }

    fn cmd_start_session(&self) {
        let tx = self.tx.clone();
        let mgr = Arc::clone(&self.mgr);
        std::thread::spawn(move || {
            if let Err(e) = mgr.start_session("") {
                let _ = tx.send(AppMsg::Session(SessionEvent::Error(format!("{e:#}"))));
            }
        });
    }

    fn cmd_finish(&self) {
        let tx = self.tx.clone();
        let mgr = Arc::clone(&self.mgr);
        let transcript = self
            .mgr
            .snapshot()
            .map(|s| s.plain_text)
            .unwrap_or_default();
        std::thread::spawn(move || {
            let msg = match mgr.finish() {
                Ok(folder) => AppMsg::SaveResult {
                    folder,
                    transcript,
                    err: None,
                },
                Err(e) => AppMsg::SaveResult {
                    folder: None,
                    transcript,
                    err: Some(format!("{e:#}")),
                },
            };
            let _ = tx.send(msg);
        });
    }

    fn cmd_generate_meta(&self, folder: PathBuf, transcript: String) {
        if self.cfg.llm_api_key.is_empty() || transcript.trim().is_empty() {
            return;
        }
        let tx = self.tx.clone();
        let mgr = Arc::clone(&self.mgr);
        let client = LlmClient::new(
            &self.cfg.llm_provider,
            &self.cfg.llm_api_key,
            &self.cfg.llm_model,
        );
        std::thread::spawn(move || {
            let msg = match client.generate_meta(&transcript) {
                Ok(meta) => {
                    let err = mgr
                        .storage
                        .save_meta(&folder, &meta.title, &meta.summary)
                        .err()
                        .map(|e| format!("{e:#}"));
                    AppMsg::MetaGenerated {
                        title: meta.title,
                        err,
                    }
                }
                Err(e) => AppMsg::MetaGenerated {
                    title: String::new(),
                    err: Some(format!("{e:#}")),
                },
            };
            let _ = tx.send(msg);
        });
    }

    fn cmd_ask(&self, user_msg: String) {
        let transcript = if self.viewing.is_some() {
            self.view_raw.clone()
        } else {
            self.mgr
                .snapshot()
                .map(|s| s.plain_text)
                .unwrap_or_default()
        };
        let system = format!(
            "You are a helpful assistant answering questions about a live meeting recording. \
             Answer concisely based on the transcript below. If the transcript is empty or the \
             answer isn't there, say so.\n\nTranscript so far:\n{transcript}"
        );
        // History excludes the just-appended user turn.
        let history: Vec<ChatMessage> = self.chat_msgs[..self.chat_msgs.len().saturating_sub(1)].to_vec();
        let client = LlmClient::new(
            &self.cfg.llm_provider,
            &self.cfg.llm_api_key,
            &self.cfg.llm_model,
        );
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let msg = match client.ask(&system, &history, &user_msg) {
                Ok(content) => AppMsg::ChatResponse { content, err: None },
                Err(e) => AppMsg::ChatResponse {
                    content: String::new(),
                    err: Some(format!("{e:#}")),
                },
            };
            let _ = tx.send(msg);
        });
    }

    pub fn kick_initial_load(&self) {
        self.cmd_load_sessions();
    }
}
