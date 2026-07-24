//! Minimal OpenAI-compatible chat client (openai / gemini / anthropic via
//! base-URL swap). Non-streaming, Go parity.

use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Serialized to chat.json with capitalized keys — the Go struct had no json
/// tags, and existing files must keep loading.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    #[serde(rename = "Role")]
    pub role: String,
    #[serde(rename = "Content")]
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct Meta {
    pub title: String,
    pub summary: String,
}

pub struct LlmClient {
    provider: String,
    api_key: String,
    model: String,
    base_url: String,
}

impl LlmClient {
    pub fn new(provider: &str, api_key: &str, model: &str) -> LlmClient {
        let base_url = match provider {
            "gemini" => "https://generativelanguage.googleapis.com/v1beta/openai".to_string(),
            "anthropic" => "https://api.anthropic.com/v1".to_string(),
            _ => "https://api.openai.com/v1".to_string(),
        };
        LlmClient {
            provider: provider.to_string(),
            api_key: api_key.to_string(),
            model: if model.is_empty() { "gpt-4o-mini".into() } else { model.into() },
            base_url,
        }
    }

    fn complete(&self, messages: Vec<serde_json::Value>, json_object: bool) -> Result<String> {
        let mut req = json!({
            "model": self.model,
            "messages": messages,
        });
        // Other providers' OpenAI-compat layers may not support response_format.
        if json_object && self.provider == "openai" {
            req["response_format"] = json!({"type": "json_object"});
        }

        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(60)))
            .http_status_as_error(false)
            .build()
            .into();
        let mut resp = agent
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", &format!("Bearer {}", self.api_key))
            .send_json(&req)?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string()?;
        if status != 200 {
            bail!("chat completion: HTTP {status}: {text}");
        }

        #[derive(Deserialize)]
        struct Completion {
            choices: Vec<Choice>,
        }
        #[derive(Deserialize)]
        struct Choice {
            message: ChoiceMessage,
        }
        #[derive(Deserialize)]
        struct ChoiceMessage {
            content: Option<String>,
        }

        let parsed: Completion =
            serde_json::from_str(&text).map_err(|e| anyhow!("parse completion: {e}"))?;
        parsed
            .choices
            .first()
            .and_then(|c| c.message.content.clone())
            .ok_or_else(|| anyhow!("no choices returned"))
    }

    pub fn ask(&self, system: &str, history: &[ChatMessage], user: &str) -> Result<String> {
        let mut messages = vec![json!({"role": "system", "content": system})];
        for m in history {
            messages.push(json!({"role": m.role, "content": m.content}));
        }
        messages.push(json!({"role": "user", "content": user}));
        self.complete(messages, false)
    }

    pub fn generate_meta(&self, transcript: &str) -> Result<Meta> {
        if transcript.is_empty() {
            bail!("transcript is empty");
        }
        let prompt = format!(
            "You are given a meeting transcript. Return ONLY a JSON object with two fields:\n\
             - \"title\": a concise meeting title, max 8 words\n\
             - \"summary\": a 2-3 sentence summary of the key points discussed\n\n\
             Example: {{\"title\": \"Q1 Planning\", \"summary\": \"The team discussed...\"}}\n\n\
             Transcript:\n{transcript}"
        );
        let content = self.complete(vec![json!({"role": "user", "content": prompt})], true)?;
        let json_str = extract_json(&content);

        #[derive(Deserialize)]
        struct MetaJson {
            #[serde(default)]
            title: String,
            #[serde(default)]
            summary: String,
        }
        let parsed: MetaJson =
            serde_json::from_str(json_str).map_err(|e| anyhow!("parse meta response: {e}"))?;
        Ok(Meta {
            title: parsed.title,
            summary: parsed.summary,
        })
    }
}

impl LlmClient {
    /// Infer real names for lettered speakers from conversational evidence
    /// ("thanks, Daniel", self-introductions). Returns letter → name for
    /// speakers with clear evidence only.
    pub fn identify_speakers(
        &self,
        transcript: &str,
    ) -> Result<std::collections::HashMap<String, String>> {
        if transcript.is_empty() {
            return Ok(Default::default());
        }
        let prompt = format!(
            "You are given a meeting transcript. Speakers are labeled \"Speaker A\", \
             \"Speaker B\", etc.; \"You\" is the person who recorded the meeting.\n\
             Identify the real names of the lettered speakers using evidence from the \
             conversation: participants addressing each other by name, self-introductions, \
             sign-offs. Return ONLY a JSON object mapping letters to names, and include a \
             letter only when the evidence is clear. Example: {{\"A\": \"Daniel\", \"B\": \"Eva\"}}. \
             Return {{}} if no names can be determined.\n\nTranscript:\n{transcript}"
        );
        let content = self.complete(vec![json!({"role": "user", "content": prompt})], true)?;
        let parsed: std::collections::HashMap<String, String> =
            serde_json::from_str(extract_json(&content))
                .map_err(|e| anyhow!("parse speaker map: {e}"))?;
        // Keep only sane entries: single-letter key, plausible short name.
        Ok(parsed
            .into_iter()
            .filter(|(k, v)| {
                k.len() == 1
                    && k.chars().all(|c| c.is_ascii_uppercase())
                    && !v.trim().is_empty()
                    && v.len() <= 40
                    && !v.contains('\n')
                    && v.trim() != k
            })
            .map(|(k, v)| (k, v.trim().to_string()))
            .collect())
    }
}

/// Grab the first '{' through the last '}' — tolerates code fences and prose.
fn extract_json(s: &str) -> &str {
    match (s.find('{'), s.rfind('}')) {
        (Some(a), Some(b)) if b > a => &s[a..=b],
        _ => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Manual e2e: identify+apply speakers on a saved session folder.
    /// YOGURT_ID_FOLDER=<path> OPENAI_API_KEY=... YOGURT_ID_MODEL=... cargo test -- --ignored identify_and_apply
    #[test]
    #[ignore]
    fn identify_and_apply_from_env() {
        let folder = std::path::PathBuf::from(std::env::var("YOGURT_ID_FOLDER").unwrap());
        let key = std::env::var("OPENAI_API_KEY").unwrap();
        let model = std::env::var("YOGURT_ID_MODEL").unwrap_or_default();
        let transcript = std::fs::read_to_string(folder.join("transcript.txt")).unwrap();
        let client = LlmClient::new("openai", &key, &model);
        let names = client.identify_speakers(&transcript).unwrap();
        eprintln!("identified: {names:?}");
        if !names.is_empty() {
            let storage = crate::session::storage::Storage::new(folder.parent().unwrap().into());
            let n = storage.apply_speaker_names(&folder, &names).unwrap();
            eprintln!("relabeled {n} segments");
        }
    }
}
