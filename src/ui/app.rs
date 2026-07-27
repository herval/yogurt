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
use crate::settings::{self, Settings};
use crate::session::{SessionEvent, SessionSummary, Status};
use crate::stt::Segment;

use super::msg::{AppMsg, ChatScope};

const CHAT_CHAR_LIMIT: usize = 500;

fn new_chat_input(placeholder: &str) -> TextArea<'static> {
    let mut ta = TextArea::default();
    ta.set_placeholder_text(placeholder.to_string());
    ta.set_cursor_line_style(ratatui::style::Style::default());
    ta
}

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

    // Summary pane
    pub summary_open: bool,
    pub summary_scroll: usize,
    pub summary_pending: bool,

    // Chat — chat_msgs always holds the current scope's conversation.
    pub chat_open: bool,
    pub chat_input: TextArea<'static>,
    pub chat_msgs: Vec<ChatMessage>,
    pub chat_scroll: usize,
    pub chat_scope: ChatScope,
    /// Scope of the in-flight ask, if any.
    chat_pending: Option<ChatScope>,
    /// Generation counter for Live scopes; bumped per recording.
    live_seq: u64,
    /// Last finished recording's (generation, folder) so a late Live reply
    /// can still land in the saved session.
    finished_live: Option<(u64, PathBuf)>,

    // Template picker
    pub templates: Vec<Template>,
    pub template_open: bool,
    pub template_idx: usize,

    // Settings page (glossary editor)
    pub settings: Settings,
    pub settings_open: bool,
    pub settings_input: TextArea<'static>,
    pub settings_tab: usize,
    pub stt_picker_open: bool,
    pub stt_idx: usize,

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
        let chat_input = new_chat_input("Ask anything...");
        let chat_msgs = mgr.storage.load_global_chat();
        let mut settings = Settings::load();
        if settings.stt_model.is_empty() {
            settings.stt_model = format!("{}/{}", cfg.stt_provider, cfg.stt_model);
        }
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
            summary_open: true,
            summary_scroll: 0,
            summary_pending: false,
            chat_open: false,
            chat_input,
            chat_msgs,
            chat_scroll: 0,
            chat_scope: ChatScope::Global,
            chat_pending: None,
            live_seq: 0,
            finished_live: None,
            templates,
            template_open: false,
            template_idx: 0,
            settings_input: new_chat_input(""),
            settings,
            settings_open: false,
            settings_tab: 0,
            stt_picker_open: false,
            stt_idx: 0,
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
                word_count,
                stt_err,
                err,
            } => {
                // Saves are async (the close pass can take a while); if a new
                // recording already started, don't yank its view or chat
                // scope — the current Live scope belongs to the new session,
                // not the one that just saved.
                let recording_again =
                    matches!(self.status, Status::Recording | Status::Paused);
                if !recording_again {
                    // Flush the live chat before the quit path below can return.
                    if let (ChatScope::Live(n), Some(f)) = (&self.chat_scope, &folder) {
                        self.finished_live = Some((*n, f.clone()));
                    }
                    self.set_chat_scope(ChatScope::Global);
                    // Batch local models deliver their transcript during finish;
                    // keep that finished transcript visible instead of jumping
                    // straight to the session list.
                    self.home_mode = !self.settings.stt_model.starts_with("parakeet/");
                }
                if let Some(e) = err {
                    self.set_notice(format!("Error saving: {e}"), true);
                } else if let Some(folder) = folder {
                    if self.quitting {
                        self.should_quit = true;
                        return;
                    }
                    if word_count == 0 {
                        // Audio is saved and recoverable (yogurt --file), but
                        // an empty transcript must not look like success.
                        let reason = stt_err
                            .unwrap_or_else(|| "no words transcribed".into());
                        self.set_notice(
                            format!("Saved audio, but transcript is EMPTY: {reason}"),
                            true,
                        );
                    } else if let Some(e) = stt_err {
                        self.set_notice(format!("Saved (transcription incomplete: {e})"), true);
                        self.cmd_generate_meta(folder, transcript);
                    } else {
                        self.set_notice("Saved — generating title & summary...", false);
                        self.cmd_generate_meta(folder, transcript);
                    }
                } else {
                    self.set_notice("Session ended (nothing to save)", false);
                }
                if self.quitting {
                    self.should_quit = true;
                }
                self.cmd_load_sessions();
            }
            AppMsg::MetaGenerated { title, speakers, err } => {
                self.summary_pending = false;
                // A visible error (e.g. "saved but transcription failed")
                // outranks the routine title announcement.
                if !self.notice_err {
                    match err {
                        Some(e) => self.set_notice(format!("Saved (could not generate title: {e})"), false),
                        None if speakers.is_empty() => {
                            self.set_notice(format!("\u{201c}{title}\u{201d}"), false)
                        }
                        None => self.set_notice(
                            format!("\u{201c}{title}\u{201d} — speakers: {}", speakers.join(", ")),
                            false,
                        ),
                    }
                }
                self.cmd_load_sessions();
            }
            AppMsg::ChatResponse { scope, content, err } => {
                if self.chat_pending.as_ref() == Some(&scope) {
                    self.chat_pending = None;
                }
                let msg = ChatMessage {
                    role: "assistant".into(),
                    content: match err {
                        Some(e) => format!("Error: {e}"),
                        None => content,
                    },
                };
                if scope == self.chat_scope {
                    self.chat_msgs.push(msg);
                    self.persist_chat();
                } else if let Some(dir) = self.chat_dir(&scope) {
                    // Late reply for a scope we've left; the user turn was
                    // persisted there when we switched away.
                    let mut msgs = self.mgr.storage.load_chat(&dir);
                    msgs.push(msg);
                    let _ = self.mgr.storage.save_chat(&dir, &msgs);
                } else {
                    self.set_notice("Chat reply discarded (recording ended without saving)", true);
                }
            }
            AppMsg::SessionsLoaded(sessions) => {
                self.sessions = sessions;
                if self.session_idx >= self.sessions.len() {
                    self.session_idx = self.sessions.len().saturating_sub(1);
                }
                // Refresh the open view so a freshly generated summary appears live.
                if let Some(v) = &self.viewing
                    && let Some(fresh) = self.sessions.iter().find(|s| s.folder == v.folder)
                {
                    self.viewing = Some(fresh.clone());
                }
            }
        }
    }

    fn handle_session_event(&mut self, ev: SessionEvent) {
        match ev {
            SessionEvent::Segment(seg) => {
                // STT delivering again means a mid-recording error (failed
                // live window) healed itself — stop showing it.
                if seg.is_final && self.notice_err && self.status == Status::Recording {
                    self.set_notice("", false);
                }
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
                        if s == Status::Idle { self.home_mode = true; }
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
        } else if self.stt_picker_open {
            self.handle_stt_picker_key(key);
        } else if self.settings_open {
            self.handle_settings_key(key);
        } else if self.chat_open {
            self.handle_chat_key(key);
        } else {
            self.handle_base_key(key);
        }
    }

    fn open_settings(&mut self) {
        let lines: Vec<String> = if self.settings.glossary.is_empty() {
            vec![String::new()]
        } else {
            self.settings.glossary.lines().map(String::from).collect()
        };
        let mut ta = TextArea::new(lines);
        ta.set_cursor_line_style(ratatui::style::Style::default());
        ta.set_placeholder_text(
            "One term or phrase per line — names, products, jargon (# for comments)",
        );
        self.settings_input = ta;
        self.settings_tab = 0;
        self.settings_open = true;
    }

    fn handle_stt_picker_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.stt_picker_open = false,
            KeyCode::Up | KeyCode::Char('k') => { self.stt_idx = self.stt_idx.saturating_sub(1); }
            KeyCode::Down | KeyCode::Char('j') => { self.stt_idx = (self.stt_idx + 1).min(settings::stt_profiles().len().saturating_sub(1)); }
            KeyCode::Enter => {
                let p = settings::stt_profiles()[self.stt_idx];
                self.settings.stt_model = p.id.to_string();
                if let Err(e) = self.settings.save() { self.set_notice(format!("Could not save STT model: {e}"), true); }
                else { self.set_notice(format!("STT model set to {}", p.label), false); }
                self.stt_picker_open = false;
            }
            _ => {}
        }
    }

    fn handle_settings_key(&mut self, key: KeyEvent) {
        if matches!(key.code, KeyCode::Left | KeyCode::Right | KeyCode::Char('1') | KeyCode::Char('2')) {
            self.settings_tab = match key.code { KeyCode::Left | KeyCode::Char('1') => 0, _ => 1 };
            return;
        }
        if self.settings_tab == 1 {
            self.handle_stt_picker_key(key);
            if key.code == KeyCode::Esc { self.settings_open = false; }
            return;
        }
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Cancel: discard edits, keep the saved glossary.
                self.settings_open = false;
            }
            KeyCode::Esc => self.save_settings(),
            _ => {
                self.settings_input.input(key);
            }
        }
    }

    fn save_settings(&mut self) {
        self.settings.glossary = self.settings_input.lines().join("\n");
        self.settings_open = false;
        let terms = self.settings.keyterms();
        let n = terms.len();
        // Applies to the next recording; also feeds the LLM prompts via
        // self.settings on the next title/summary/chat call.
        self.mgr.set_keyterms(terms);
        match self.settings.save() {
            Ok(()) => self.set_notice(
                format!("Glossary saved — {n} term{}", if n == 1 { "" } else { "s" }),
                false,
            ),
            Err(e) => self.set_notice(format!("Could not save glossary: {e}"), true),
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
                if !text.is_empty() && !self.chat_loading() {
                    self.chat_msgs.push(ChatMessage {
                        role: "user".into(),
                        content: text.clone(),
                    });
                    self.chat_input = new_chat_input(self.chat_placeholder());
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

    pub fn chat_loading(&self) -> bool {
        self.chat_pending.as_ref() == Some(&self.chat_scope)
    }

    fn chat_placeholder(&self) -> &'static str {
        match self.chat_scope {
            ChatScope::Global => "Ask anything...",
            _ => "Ask about the transcript...",
        }
    }

    /// Where a scope's chat lives on disk, if anywhere.
    fn chat_dir(&self, scope: &ChatScope) -> Option<PathBuf> {
        match scope {
            ChatScope::Global => Some(self.mgr.storage.base_dir.clone()),
            ChatScope::Session(f) if f.exists() => Some(f.clone()),
            ChatScope::Session(_) => None, // deleted since
            ChatScope::Live(n) => self
                .finished_live
                .as_ref()
                .filter(|(m, _)| m == n)
                .map(|(_, f)| f.clone()),
        }
    }

    fn persist_chat(&self) {
        if self.chat_msgs.is_empty() {
            return;
        }
        if self.chat_scope == ChatScope::Global {
            let _ = self.mgr.storage.save_global_chat(&self.chat_msgs);
        } else if let Some(dir) = self.chat_dir(&self.chat_scope) {
            let _ = self.mgr.storage.save_chat(&dir, &self.chat_msgs);
        }
    }

    /// The single scope-transition funnel: save the outgoing conversation,
    /// load the incoming one. chat_pending stays — it tags the request.
    fn set_chat_scope(&mut self, scope: ChatScope) {
        if scope == self.chat_scope {
            return;
        }
        self.persist_chat();
        self.chat_scope = scope;
        self.chat_msgs = match &self.chat_scope {
            ChatScope::Session(f) => self.mgr.storage.load_chat(f),
            ChatScope::Global => self.mgr.storage.load_global_chat(),
            ChatScope::Live(_) => Vec::new(),
        };
        self.chat_scroll = 0;
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
                        self.set_chat_scope(ChatScope::Session(PathBuf::from(&s.folder)));
                        self.viewing = Some(s);
                        self.scroll = 0;
                        self.summary_open = true;
                        self.summary_scroll = 0;
                        self.summary_pending = false;
                    }
                }
            }
            KeyCode::Esc => {
                if self.viewing.is_none()
                    && matches!(self.status, Status::Recording | Status::Paused)
                {
                    self.back_to_live();
                } else {
                    self.viewing = None;
                    self.chat_open = false;
                    self.set_chat_scope(ChatScope::Global);
                }
            }
            KeyCode::Char('c') | KeyCode::Char('C') => self.open_chat(),
            KeyCode::Char('s') | KeyCode::Char('S') => {
                if self.viewing.is_some() {
                    self.toggle_summary();
                } else {
                    self.open_settings();
                }
            }
            KeyCode::PageUp => {
                if self.viewing.is_some() {
                    self.summary_scroll = self.summary_scroll.saturating_sub(1);
                }
            }
            KeyCode::PageDown => {
                if self.viewing.is_some() {
                    self.summary_scroll += 1;
                }
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                // A session is already live — jump back to it instead of
                // wiping the live view's state for a doomed start attempt.
                if matches!(self.status, Status::Recording | Status::Paused) {
                    self.back_to_live();
                } else {
                    self.new_session();
                }
            }
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
            KeyCode::Esc => {
                if matches!(self.status, Status::Finished | Status::Idle) {
                    self.home_mode = true;
                    self.viewing = None;
                    self.set_chat_scope(ChatScope::Global);
                }
            }
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
            KeyCode::Char('x') | KeyCode::Char('X') => {
                if matches!(self.status, Status::Recording | Status::Paused) {
                    self.cmd_cancel_session();
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
        self.chat_input.set_placeholder_text(self.chat_placeholder());
    }

    fn toggle_summary(&mut self) {
        let Some(viewing) = &self.viewing else { return };
        if !viewing.summary.is_empty() {
            self.summary_open = !self.summary_open;
            return;
        }
        let folder = PathBuf::from(&viewing.folder);
        // No summary yet: generate on demand (legacy sessions, failed runs).
        if self.cfg.llm_api_key.is_empty() {
            let provider = self.cfg.llm_provider.to_uppercase();
            self.set_notice(
                format!("Set LLM_MODEL and {provider}_API_KEY to enable summaries"),
                true,
            );
            return;
        }
        if self.view_raw.trim().is_empty() {
            self.set_notice("Transcript is empty — nothing to summarize", true);
            return;
        }
        if !self.summary_pending {
            self.summary_pending = true;
            self.summary_open = true;
            self.set_notice("Generating summary...", false);
            self.cmd_generate_meta(folder, self.view_raw.clone());
        }
    }

    /// Return from the home list to an in-progress recording's live view.
    fn back_to_live(&mut self) {
        self.home_mode = false;
        self.viewing = None;
        self.chat_open = false;
        self.set_chat_scope(ChatScope::Live(self.live_seq));
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
        self.live_seq += 1;
        self.set_chat_scope(ChatScope::Live(self.live_seq));
        self.cmd_start_session();
    }

    fn quit(&mut self) {
        if matches!(self.status, Status::Recording | Status::Paused) {
            self.quitting = true;
            self.cmd_finish();
        } else {
            // Keep a just-typed user turn; its reply can't arrive anymore.
            self.persist_chat();
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
        let profile = settings::stt_profile(&self.settings.stt_model);
        let (provider, model, key) = profile.map(|p| (p.provider.to_string(), p.model.to_string(), self.cfg.api_key_for(p.provider)))
            .unwrap_or_else(|| (self.cfg.stt_provider.clone(), self.cfg.stt_model.clone(), self.cfg.stt_api_key.clone()));
        std::thread::spawn(move || {
            if let Err(e) = mgr.start_session_with_stt("", &provider, &key, &model) {
                let _ = tx.send(AppMsg::Session(SessionEvent::Error(format!("{e:#}"))));
            }
        });
    }

    fn cmd_finish(&self) {
        let tx = self.tx.clone();
        let mgr = Arc::clone(&self.mgr);
        std::thread::spawn(move || {
            let msg = match mgr.finish() {
                Ok(Some(o)) => AppMsg::SaveResult {
                    folder: Some(o.folder),
                    transcript: o.transcript,
                    word_count: o.word_count,
                    stt_err: o.stt_error,
                    err: None,
                },
                Ok(None) => AppMsg::SaveResult {
                    folder: None,
                    transcript: String::new(),
                    word_count: 0,
                    stt_err: None,
                    err: None,
                },
                Err(e) => AppMsg::SaveResult {
                    folder: None,
                    transcript: String::new(),
                    word_count: 0,
                    stt_err: None,
                    err: Some(format!("{e:#}")),
                },
            };
            let _ = tx.send(msg);
        });
    }

    fn cmd_cancel_session(&self) {
        let mgr = Arc::clone(&self.mgr);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            mgr.cancel();
            let _ = tx.send(AppMsg::Session(SessionEvent::Status(Status::Idle)));
            let _ = tx.send(AppMsg::Session(SessionEvent::Notice("Recording discarded".into())));
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
        )
        .with_glossary(self.settings.llm_prompt());
        std::thread::spawn(move || {
            let msg = match client.generate_meta(&transcript) {
                Ok(meta) => {
                    let err = mgr
                        .storage
                        .save_meta(&folder, &meta.title, &meta.summary)
                        .err()
                        .map(|e| format!("{e:#}"));
                    // Identify speakers by name from conversational evidence
                    // and rewrite the saved transcript. Best-effort.
                    let mut speakers = Vec::new();
                    match client.identify_speakers(&transcript) {
                        Ok(names) if !names.is_empty() => {
                            match mgr.storage.apply_speaker_names(&folder, &names) {
                                Ok(n) if n > 0 => {
                                    speakers = names.values().cloned().collect();
                                    speakers.sort();
                                }
                                Ok(_) => {}
                                Err(e) => log::warn!("apply speaker names: {e:#}"),
                            }
                        }
                        Ok(_) => {}
                        Err(e) => log::warn!("identify speakers: {e:#}"),
                    }
                    AppMsg::MetaGenerated {
                        title: meta.title,
                        speakers,
                        err,
                    }
                }
                Err(e) => AppMsg::MetaGenerated {
                    title: String::new(),
                    speakers: Vec::new(),
                    err: Some(format!("{e:#}")),
                },
            };
            let _ = tx.send(msg);
        });
    }

    fn cmd_ask(&mut self, user_msg: String) {
        let scope = self.chat_scope.clone();
        self.chat_pending = Some(scope.clone());
        let system = match &scope {
            ChatScope::Global => "You are a helpful assistant inside a meeting-recorder app. \
                 No recording is currently open. Answer concisely."
                .to_string(),
            _ => {
                let transcript = if self.viewing.is_some() {
                    self.view_raw.clone()
                } else {
                    self.mgr
                        .snapshot()
                        .map(|s| s.plain_text)
                        .unwrap_or_default()
                };
                format!(
                    "You are a helpful assistant answering questions about a live meeting recording. \
                     Answer concisely based on the transcript below. If the transcript is empty or the \
                     answer isn't there, say so.\n\nTranscript so far:\n{transcript}"
                )
            }
        };
        // History excludes the just-appended user turn.
        let history: Vec<ChatMessage> = self.chat_msgs[..self.chat_msgs.len().saturating_sub(1)].to_vec();
        let client = LlmClient::new(
            &self.cfg.llm_provider,
            &self.cfg.llm_api_key,
            &self.cfg.llm_model,
        )
        .with_glossary(self.settings.llm_prompt());
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let msg = match client.ask(&system, &history, &user_msg) {
                Ok(content) => AppMsg::ChatResponse {
                    scope,
                    content,
                    err: None,
                },
                Err(e) => AppMsg::ChatResponse {
                    scope,
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
