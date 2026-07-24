mod audio;
mod config;
mod disclaim;
mod llm;
mod session;
mod settings;
mod stt;
mod ui;

use std::path::PathBuf;
use std::sync::mpsc;

use anyhow::Result;
use clap::Parser;

use config::Config;
use session::SessionEvent;
use session::manager::{Manager, ManagerConfig};

#[derive(Parser)]
#[command(name = "yogurt", about = "Meeting recorder & transcriber")]
struct Cli {
    /// List available audio input devices and exit
    #[arg(long)]
    list_devices: bool,

    /// Audio input device index (-1 = default)
    #[arg(long, default_value_t = -1)]
    device: i32,

    /// Directory to save sessions (overrides YOGURT_SESSIONS_DIR)
    #[arg(long)]
    sessions_dir: Option<String>,

    /// Path to a WAV/MP3 file to transcribe (skips live recording)
    #[arg(long)]
    file: Option<PathBuf>,

    /// Session name (used with --file; defaults to filename)
    #[arg(long)]
    name: Option<String>,

    /// Record 2 seconds from the mic and report the peak level, then exit
    #[arg(long)]
    mic_check: bool,

    /// Transcribe and save recordings recovered from a crash, then exit
    #[arg(long)]
    recover: bool,

    /// Capture 3 seconds of system audio and report the peak level, then exit
    #[arg(long)]
    sys_check: bool,
}

fn main() {
    // Must run before anything TCC-related: re-exec as our own responsible process.
    disclaim::maybe_reexec_disclaimed();

    let cli = Cli::parse();
    let mut cfg = Config::from_env();

    if let Some(dir) = &cli.sessions_dir {
        cfg.sessions_dir = dir.clone();
    }
    if cli.device >= 0 {
        cfg.audio_device = cli.device;
    }

    if cli.list_devices {
        ensure_permission();
        let devices = audio::capture::list_devices();
        if devices.is_empty() {
            println!("No audio input devices found.");
            std::process::exit(1);
        }
        println!("Available audio input devices:");
        for d in &devices {
            println!("  [{}] {}", d.index, d.name);
        }
        return;
    }

    if cli.mic_check {
        ensure_permission();
        if let Err(e) = mic_check(&cfg) {
            eprintln!("Error: {e:#}");
            std::process::exit(1);
        }
        return;
    }

    if cli.sys_check {
        if let Err(e) = sys_check(&cfg) {
            eprintln!("Error: {e:#}");
            std::process::exit(1);
        }
        return;
    }

    let errs = cfg.validate();
    if !errs.is_empty() {
        for e in errs {
            eprintln!("Error: {e}");
        }
        std::process::exit(1);
    }

    let (events_tx, events_rx) = mpsc::channel::<SessionEvent>();
    let notice_tx = events_tx.clone();
    let mgr = Manager::new(
        ManagerConfig {
            stt_provider: cfg.stt_provider.clone(),
            stt_api_key: cfg.stt_api_key.clone(),
            stt_model: cfg.stt_model.clone(),
            sample_rate: cfg.sample_rate,
            device_index: cfg.audio_device,
            sessions_dir: cfg.abs_sessions_dir(),
            keyterms: settings::Settings::load().keyterms(),
        },
        events_tx,
    );

    // Headless crash recovery
    if cli.recover {
        init_stderr_logging();
        std::thread::spawn(move || {
            for ev in events_rx {
                if let SessionEvent::Error(e) | SessionEvent::Notice(e) = ev {
                    eprintln!("{e}");
                }
            }
        });
        let results = mgr.recover_spools();
        if results.is_empty() {
            println!("No crashed recordings found.");
            return;
        }
        let mut failed = false;
        for (id, outcome) in results {
            match outcome {
                Ok(folder) => {
                    println!("Recovered {id} -> {}", folder.display());
                    generate_meta_headless(&cfg, &mgr.storage, &folder);
                }
                Err(e) => {
                    failed = true;
                    eprintln!("Failed to recover {id}: {e:#}");
                }
            }
        }
        if failed {
            std::process::exit(1);
        }
        return;
    }

    // Headless --file mode
    if let Some(file) = &cli.file {
        init_stderr_logging();
        let name = cli.name.clone().unwrap_or_else(|| {
            file.file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "recording".to_string())
        });
        println!("Transcribing {} as {:?}...", file.display(), name);

        // Consume events so error/notice lines reach stderr.
        std::thread::spawn(move || {
            for ev in events_rx {
                if let SessionEvent::Error(e) | SessionEvent::Notice(e) = ev {
                    eprintln!("{e}");
                }
            }
        });

        let mut last_pct = 0usize;
        let result = mgr.transcribe_file(&name, file, &mut |sent, total| {
            let pct = sent * 100 / total.max(1);
            if pct / 10 != last_pct / 10 {
                last_pct = pct;
                println!("  sending audio... {pct}%");
            }
        });
        match result {
            Ok(folder) => {
                println!("Done. Session saved to: {}", folder.display());
                generate_meta_headless(&cfg, &mgr.storage, &folder);
            }
            Err(e) => {
                eprintln!("Error: {e:#}");
                std::process::exit(1);
            }
        }
        return;
    }

    // TUI mode: log to a file so output doesn't corrupt the alt-screen.
    init_file_logging();
    log::info!(
        "starting yogurt (device={}, sampleRate={}, stt={}/{}, llm={}/{})",
        cfg.audio_device,
        cfg.sample_rate,
        cfg.stt_provider,
        cfg.stt_model,
        cfg.llm_provider,
        cfg.llm_model
    );

    ensure_permission();
    let devices = audio::capture::list_devices();

    let pending = mgr.pending_spools();
    if pending > 0 {
        let _ = notice_tx.send(SessionEvent::Notice(format!(
            "{pending} crashed recording(s) found — run: yogurt --recover"
        )));
    }

    let templates_path = dirs::home_dir()
        .unwrap_or_default()
        .join(".yogurt")
        .join("chat_templates.json");
    let templates = llm::templates::ensure_templates_file(&templates_path);

    if let Err(e) = ui::run(mgr, events_rx, devices, cfg, templates) {
        eprintln!("Error: {e:#}");
        std::process::exit(1);
    }
}

/// Best-effort title/summary generation for headless saves (--file, --recover).
/// Failures are non-fatal: the transcription itself already succeeded.
fn generate_meta_headless(
    cfg: &Config,
    storage: &session::storage::Storage,
    folder: &std::path::Path,
) {
    if cfg.llm_api_key.is_empty() {
        return;
    }
    let transcript = storage.load_transcript(folder);
    if transcript.trim().is_empty() {
        return;
    }
    println!("Generating title & summary...");
    let client = llm::client::LlmClient::new(&cfg.llm_provider, &cfg.llm_api_key, &cfg.llm_model)
        .with_glossary(settings::Settings::load().llm_prompt());
    match client.generate_meta(&transcript) {
        Ok(meta) => match storage.save_meta(folder, &meta.title, &meta.summary) {
            Ok(()) => println!("  \u{201c}{}\u{201d}", meta.title),
            Err(e) => eprintln!("Could not save summary: {e:#}"),
        },
        Err(e) => eprintln!("Could not generate summary: {e:#}"),
    }

    // Put names on diarized speakers where the conversation supports it.
    match client.identify_speakers(&transcript) {
        Ok(names) if !names.is_empty() => match storage.apply_speaker_names(folder, &names) {
            Ok(n) if n > 0 => {
                let mut who: Vec<&str> = names.values().map(|s| s.as_str()).collect();
                who.sort();
                println!("  speakers: {}", who.join(", "));
            }
            Ok(_) => {}
            Err(e) => eprintln!("Could not apply speaker names: {e:#}"),
        },
        Ok(_) => {}
        Err(e) => eprintln!("Could not identify speakers: {e:#}"),
    }
}

fn mic_check(cfg: &Config) -> Result<()> {
    use std::time::{Duration, Instant};
    let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(512);
    let capture = audio::capture::Capture::start(cfg.audio_device, cfg.sample_rate, tx)?;
    println!("Recording 2 seconds from the mic...");
    let start = Instant::now();
    let mut bytes = 0usize;
    let mut peak = 0.0f64;
    while start.elapsed() < Duration::from_secs(2) {
        if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(200)) {
            bytes += chunk.len();
            peak = peak.max(audio::calc_level(&chunk));
        }
    }
    drop(capture);
    println!("Captured {bytes} bytes, peak level {peak:.2}");
    if bytes == 0 {
        anyhow::bail!("no audio captured — check mic permission and device");
    }
    Ok(())
}

fn sys_check(cfg: &Config) -> Result<()> {
    use std::time::{Duration, Instant};
    let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(512);
    let tap = audio::system_tap::SystemTap::start(cfg.sample_rate, tx)?;
    println!("Capturing 3 seconds of system audio (play something!)...");
    let start = Instant::now();
    let mut bytes = 0usize;
    let mut peak = 0.0f64;
    while start.elapsed() < Duration::from_secs(3) {
        if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(200)) {
            bytes += chunk.len();
            peak = peak.max(audio::calc_level(&chunk));
        }
    }
    drop(tap);
    println!("Captured {bytes} bytes of system audio, peak level {peak:.2}");
    if bytes == 0 {
        anyhow::bail!("no system audio captured — check the System Audio Recording permission");
    }
    Ok(())
}

fn ensure_permission() {
    let status = audio::capture::authorization_status();
    if status == 2 {
        eprintln!("Microphone access denied.");
        eprintln!("Enable it in: System Settings → Privacy & Security → Microphone");
        std::process::exit(1);
    }
    if status != 3 {
        // Blocks until the user responds to the dialog.
        if let Err(e) = audio::capture::request_permission() {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

fn init_stderr_logging() {
    let _ = simplelog::TermLogger::init(
        simplelog::LevelFilter::Info,
        simplelog::Config::default(),
        simplelog::TerminalMode::Stderr,
        simplelog::ColorChoice::Never,
    );
}

fn init_file_logging() {
    if let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("yogurt.log")
    {
        let _ = simplelog::WriteLogger::init(
            simplelog::LevelFilter::Info,
            simplelog::Config::default(),
            file,
        );
    }
}
