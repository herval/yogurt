use std::path::PathBuf;

use crate::session::{SessionEvent, SessionSummary};

/// Which conversation the chat pane is showing / a reply belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatScope {
    /// Home list, no session open. Persisted at base_dir/chat.json.
    Global,
    /// A stored session, keyed by its folder.
    Session(PathBuf),
    /// A live recording with no folder yet. The generation counter keeps a
    /// reply from recording N out of recording N+1.
    Live(u64),
}

/// Everything the UI thread reacts to — the bubbletea Msg analog.
pub enum AppMsg {
    Key(crossterm::event::KeyEvent),
    Resize,
    Tick,
    Session(SessionEvent),
    SaveResult {
        folder: Option<PathBuf>,
        transcript: String,
        /// Words in the saved transcript; 0 means STT produced nothing.
        word_count: usize,
        /// STT failure during close — the audio still saved.
        stt_err: Option<String>,
        err: Option<String>,
    },
    MetaGenerated {
        title: String,
        speakers: Vec<String>,
        err: Option<String>,
    },
    ChatResponse {
        scope: ChatScope,
        content: String,
        err: Option<String>,
    },
    SessionsLoaded(Vec<SessionSummary>),
}
