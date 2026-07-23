//! Ratatui TUI. M4.

use std::sync::Arc;
use std::sync::mpsc::Receiver;

use anyhow::{Result, bail};

use crate::config::Config;
use crate::llm::templates::Template;
use crate::session::SessionEvent;
use crate::session::manager::Manager;

pub fn run(
    _mgr: Arc<Manager>,
    _events: Receiver<SessionEvent>,
    _devices: Vec<crate::audio::Device>,
    _cfg: Config,
    _templates: Vec<Template>,
) -> Result<()> {
    bail!("TUI not yet implemented (M4) — use --file for headless transcription")
}
