use std::path::PathBuf;

use crate::session::{SessionEvent, SessionSummary};

/// Everything the UI thread reacts to — the bubbletea Msg analog.
pub enum AppMsg {
    Key(crossterm::event::KeyEvent),
    Resize,
    Tick,
    Session(SessionEvent),
    SaveResult {
        folder: Option<PathBuf>,
        transcript: String,
        err: Option<String>,
    },
    MetaGenerated {
        title: String,
        err: Option<String>,
    },
    ChatResponse {
        content: String,
        err: Option<String>,
    },
    SessionsLoaded(Vec<SessionSummary>),
}
