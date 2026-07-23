mod app;
mod markdown;
mod msg;
mod view;
mod wrap;

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use anyhow::Result;

use crate::config::Config;
use crate::llm::templates::Template;
use crate::session::SessionEvent;
use crate::session::manager::Manager;

use app::App;
use msg::AppMsg;

pub fn run(
    mgr: Arc<Manager>,
    events: Receiver<SessionEvent>,
    devices: Vec<crate::audio::Device>,
    cfg: Config,
    templates: Vec<Template>,
) -> Result<()> {
    let mut terminal = ratatui::init(); // installs a panic hook that restores the terminal

    let (tx, rx) = mpsc::channel::<AppMsg>();

    // Input thread
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            loop {
                match crossterm::event::read() {
                    Ok(crossterm::event::Event::Key(key))
                        if key.kind == crossterm::event::KeyEventKind::Press =>
                    {
                        if tx.send(AppMsg::Key(key)).is_err() {
                            return;
                        }
                    }
                    Ok(crossterm::event::Event::Resize(_, _)) => {
                        if tx.send(AppMsg::Resize).is_err() {
                            return;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => return,
                }
            }
        });
    }

    // 1s tick (duration display)
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(1));
                if tx.send(AppMsg::Tick).is_err() {
                    return;
                }
            }
        });
    }

    // Session events forwarder
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for ev in events {
                if tx.send(AppMsg::Session(ev)).is_err() {
                    return;
                }
            }
        });
    }

    let mut app = App::new(mgr, devices, cfg, templates, tx);
    app.kick_initial_load();

    let result = (|| -> Result<()> {
        loop {
            terminal.draw(|f| view::draw(f, &mut app))?;
            let Ok(msg) = rx.recv() else { break };
            app.update(msg);
            // Coalesce bursts (audio levels, segments) before redrawing.
            while let Ok(m) = rx.try_recv() {
                app.update(m);
                if app.should_quit {
                    break;
                }
            }
            if app.should_quit {
                break;
            }
        }
        Ok(())
    })();

    ratatui::restore();
    result
}
