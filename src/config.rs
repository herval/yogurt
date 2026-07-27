use std::env;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Config {
    pub stt_provider: String,
    pub stt_model: String,
    pub stt_api_key: String,

    pub llm_provider: String,
    pub llm_model: String,
    pub llm_api_key: String,

    pub sessions_dir: String,
    pub sample_rate: u32,
    pub audio_device: i32, // -1 = default
    pub device_name: String,
}

impl Config {
    pub fn from_env() -> Config {
        let _ = dotenvy::dotenv();

        let mut cfg = Config {
            stt_provider: String::new(),
            stt_model: String::new(),
            stt_api_key: String::new(),
            llm_provider: String::new(),
            llm_model: String::new(),
            llm_api_key: String::new(),
            sessions_dir: env_or_default("YOGURT_SESSIONS_DIR", "./sessions"),
            sample_rate: 16000,
            audio_device: -1,
            device_name: String::new(),
        };

        let mut stt_raw = env::var("STT_MODEL").unwrap_or_default();
        if stt_raw.is_empty() {
            // Legacy fallback
            stt_raw = format!(
                "assemblyai/{}",
                env_or_default("YOGURT_SPEECH_MODEL", "universal-streaming-multilingual")
            );
        }
        (cfg.stt_provider, cfg.stt_model) = parse_provider_model(&stt_raw, "assemblyai");
        cfg.stt_api_key = provider_api_key(&cfg.stt_provider);

        let mut llm_raw = env::var("LLM_MODEL").unwrap_or_default();
        if llm_raw.is_empty() {
            llm_raw = format!("openai/{}", env_or_default("OPENAI_MODEL", "gpt-4o-mini"));
        }
        (cfg.llm_provider, cfg.llm_model) = parse_provider_model(&llm_raw, "openai");
        cfg.llm_api_key = provider_api_key(&cfg.llm_provider);

        if let Ok(sr) = env::var("YOGURT_SAMPLE_RATE") {
            if let Ok(n) = sr.parse::<u32>() {
                cfg.sample_rate = n;
            }
        }
        if let Ok(dev) = env::var("YOGURT_AUDIO_DEVICE") {
            match dev.parse::<i32>() {
                Ok(idx) => cfg.audio_device = idx,
                Err(_) => cfg.device_name = dev,
            }
        }

        cfg
    }

    pub fn validate(&self) -> Vec<String> {
        let mut errs = Vec::new();
        if self.stt_api_key.is_empty()
            && !matches!(self.stt_provider.as_str(), "whisper" | "parakeet")
        {
            errs.push(format!(
                "missing STT API key: set {}_API_KEY (or use STT_MODEL=whisper/<model> or parakeet/<model>)",
                self.stt_provider.to_uppercase()
            ));
        }
        if ![8000, 16000, 22050, 44100, 48000].contains(&self.sample_rate) {
            errs.push(format!(
                "invalid YOGURT_SAMPLE_RATE {} (use 8000, 16000, 22050, 44100 or 48000)",
                self.sample_rate
            ));
        }
        errs
    }

    pub fn set_stt_model(&mut self, raw: &str) {
        (self.stt_provider, self.stt_model) = parse_provider_model(raw, "assemblyai");
        self.stt_api_key = provider_api_key(&self.stt_provider);
    }

    pub fn api_key_for(&self, provider: &str) -> String { provider_api_key(provider) }

    pub fn abs_sessions_dir(&self) -> PathBuf {
        let p = Path::new(&self.sessions_dir);
        let abs = if p.is_absolute() {
            p.to_path_buf()
        } else {
            env::current_dir()
                .map(|cwd| cwd.join(p))
                .unwrap_or_else(|_| p.to_path_buf())
        };
        // Drop "." components (filepath.Abs in Go cleans these).
        abs.components()
            .filter(|c| !matches!(c, std::path::Component::CurDir))
            .collect()
    }
}

/// "provider/model" → (provider lowercased, model). No slash → (default, s).
fn parse_provider_model(s: &str, default_provider: &str) -> (String, String) {
    match s.split_once('/') {
        Some((p, m)) => (p.to_lowercase(), m.to_string()),
        None => (default_provider.to_string(), s.to_string()),
    }
}

fn provider_api_key(provider: &str) -> String {
    env::var(format!("{}_API_KEY", provider.to_uppercase())).unwrap_or_default()
}

fn env_or_default(key: &str, default: &str) -> String {
    match env::var(key) {
        Ok(v) if !v.is_empty() => v,
        _ => default.to_string(),
    }
}
