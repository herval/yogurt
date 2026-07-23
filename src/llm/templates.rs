use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Template {
    pub name: String,
    pub prompt: String,
}

fn default_templates() -> Vec<Template> {
    let t = |name: &str, prompt: &str| Template {
        name: name.to_string(),
        prompt: prompt.to_string(),
    };
    vec![
        t(
            "TL;DR",
            "Give me a brief summary (tl;dr) of this recording in 2-3 sentences.",
        ),
        t(
            "What did I miss?",
            "What were the most important points discussed? List the key decisions, action items, or insights I should know about.",
        ),
        t(
            "Action items",
            "List all action items, tasks, or commitments mentioned in this recording. Include who is responsible if mentioned.",
        ),
        t(
            "Key decisions",
            "What decisions were made during this meeting or conversation? List them clearly.",
        ),
        t(
            "Open questions",
            "What questions were raised but not resolved? List any unresolved topics or follow-up items.",
        ),
    ]
}

pub fn load_templates(path: &Path) -> anyhow::Result<Vec<Template>> {
    let data = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&data)?)
}

/// Load templates, creating the file with defaults on any failure (Go parity:
/// malformed JSON also resets to defaults).
pub fn ensure_templates_file(path: &Path) -> Vec<Template> {
    if let Ok(templates) = load_templates(path) {
        return templates;
    }
    let defaults = default_templates();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(&defaults) {
        let _ = std::fs::write(path, json);
    }
    defaults
}
