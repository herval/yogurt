use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::session::Status;
use crate::settings;

use super::app::App;
use super::markdown::render_markdown;
use super::msg::ChatScope;
use super::wrap::{truncate, word_wrap};

const TITLE_STYLE: Style = Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD);
const SPEAKER_STYLE: Style = Style::new().fg(Color::Green).add_modifier(Modifier::BOLD);
const TIME_STYLE: Style = Style::new().fg(Color::Cyan);
const DIM: Style = Style::new().add_modifier(Modifier::DIM);
const ERROR_STYLE: Style = Style::new().fg(Color::Red);
const NOTICE_STYLE: Style = Style::new().fg(Color::Yellow);
const USER_STYLE: Style = Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD);
const AI_STYLE: Style = Style::new().fg(Color::Magenta).add_modifier(Modifier::BOLD);

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    if area.width < 10 || area.height < 8 {
        f.render_widget(Paragraph::new("Terminal too small"), area);
        return;
    }

    if app.selecting_mic {
        draw_mic_select(f, app, area);
        return;
    }

    let [header, pane, status_bar, controls, notice] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);

    draw_header(f, app, header);

    if app.settings_open {
        draw_settings(f, app, pane);
    } else if app.home_mode {
        draw_home(f, app, pane);
    } else {
        draw_live(f, app, pane);
    }

    draw_status_bar(f, app, status_bar);
    draw_controls(f, app, controls);
    draw_notice(f, app, notice);

    if app.template_open {
        draw_template_modal(f, app, area);
    }
    if app.confirm_delete {
        draw_confirm_delete(f, app, area);
    }
    if app.stt_picker_open {
        draw_stt_picker(f, app, area);
    }
}

fn draw_stt_picker(f: &mut Frame, app: &App, area: Rect) {
    let w = area.width.min(80); let h = (settings::stt_profiles().len() as u16 + 5).min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2; let y = area.y + (area.height.saturating_sub(h)) / 2;
    let rect = Rect::new(x, y, w, h);
    f.render_widget(Clear, rect);
    let mut lines = vec![Line::from(Span::styled("Choose the model for new recordings", TITLE_STYLE))];
    for (i, p) in settings::stt_profiles().iter().enumerate() {
        let marker = if i == app.stt_idx { "▶" } else { " " };
        let availability = if p.local { if settings::stt_profile_available(*p) { "installed" } else { "not installed" } } else { "API" };
        lines.push(Line::from(format!("{marker} {:<40} [{}]", p.label, availability)));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("↑/↓ select   Enter save   Esc cancel", DIM)));
    f.render_widget(Paragraph::new(lines).block(Block::default().title(" STT model ").borders(Borders::ALL)), rect);
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let (indicator, style) = match app.status {
        Status::Recording => ("● RECORDING", Style::new().fg(Color::Red).bold()),
        Status::Paused => ("⏸ PAUSED", Style::new().fg(Color::Yellow).bold()),
        Status::Finished => ("○ FINISHED", DIM),
        Status::Idle => ("○ IDLE", DIM),
    };
    let title = "YOGURT - Meeting Recorder";
    let pad = (area.width as usize)
        .saturating_sub(title.len() + indicator.chars().count() + 1);
    let line = Line::from(vec![
        Span::styled(title, TITLE_STYLE),
        Span::raw(" ".repeat(pad.max(1))),
        Span::styled(indicator, style),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn transcript_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line> = Vec::new();
    for tl in &app.lines {
        let mut header = vec![
            Span::styled(format!("[{}]", tl.seg.format_timestamp()), TIME_STYLE),
            Span::raw(" "),
            Span::styled(
                if tl.seg.speaker.is_empty() {
                    "Unknown".to_string()
                } else if tl.seg.speaker.len() == 1 {
                    format!("Speaker {}", tl.seg.speaker)
                } else {
                    tl.seg.speaker.clone()
                },
                SPEAKER_STYLE,
            ),
        ];
        if tl.partial {
            header.push(Span::styled(" (partial)", DIM));
        }
        out.push(Line::from(header));
        let body_style = if tl.partial { DIM } else { Style::default() };
        for l in word_wrap(&tl.seg.text, width.saturating_sub(2)) {
            out.push(Line::from(Span::styled(format!("  {l}"), body_style)));
        }
        out.push(Line::default());
    }
    out
}

fn windowed<'a>(lines: Vec<Line<'a>>, available: usize, scroll: usize) -> Vec<Line<'a>> {
    let total = lines.len();
    let end = total.saturating_sub(scroll).max(available.min(total));
    let start = end.saturating_sub(available);
    lines[start..end].to_vec()
}

fn draw_live(f: &mut Frame, app: &mut App, area: Rect) {
    if app.chat_open {
        let [left, right] =
            Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
                .areas(area);
        draw_transcript_pane(f, app, left);
        draw_chat_pane(f, app, right);
    } else {
        draw_transcript_pane(f, app, area);
    }
}

fn draw_transcript_pane(f: &mut Frame, app: &App, area: Rect) {
    let inner_w = area.width.saturating_sub(4) as usize;
    let available = area.height.saturating_sub(2) as usize;
    let lines = transcript_lines(app, inner_w);
    let content = windowed(lines, available, app.scroll);
    let block = Block::default().borders(Borders::ALL);
    f.render_widget(Paragraph::new(content).block(block), area);
}

fn draw_chat_pane(f: &mut Frame, app: &mut App, area: Rect) {
    let title = if app.chat_scope == ChatScope::Global {
        " Chat — Global "
    } else {
        " Chat "
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let [msgs_area, divider, input_area, hint] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);

    let width = inner.width.saturating_sub(1) as usize;
    let mut lines: Vec<Line> = Vec::new();
    for m in &app.chat_msgs {
        if m.role == "user" {
            lines.push(Line::from(Span::styled("You:", USER_STYLE)));
            for l in word_wrap(&m.content, width) {
                lines.push(Line::raw(l));
            }
        } else {
            lines.push(Line::from(Span::styled("AI:", AI_STYLE)));
            lines.extend(render_markdown(&m.content, width));
        }
        lines.push(Line::default());
    }
    if app.chat_loading() {
        lines.push(Line::from(Span::styled("thinking...", DIM)));
    }
    let content = windowed(lines, msgs_area.height as usize, app.chat_scroll);
    f.render_widget(Paragraph::new(content), msgs_area);

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(divider.width as usize),
            DIM,
        ))),
        divider,
    );
    f.render_widget(&app.chat_input, input_area);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Enter to send • ? for templates • Esc to close",
            DIM,
        ))),
        hint,
    );
}

fn draw_summary_pane(f: &mut Frame, app: &mut App, summary: &str, area: Rect) {
    let block = Block::default().borders(Borders::ALL).title(" Summary ");
    let inner_w = area.width.saturating_sub(2) as usize;
    let lines: Vec<Line> = if summary.is_empty() {
        vec![Line::from(Span::styled("Generating summary...", DIM))]
    } else {
        render_markdown(summary, inner_w)
    };
    // Top-anchored scroll, clamped so the last line stays reachable.
    let visible = area.height.saturating_sub(2) as usize;
    let max_scroll = lines.len().saturating_sub(visible);
    app.summary_scroll = app.summary_scroll.min(max_scroll);
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .scroll((app.summary_scroll as u16, 0)),
        area,
    );
}

fn draw_home(f: &mut Frame, app: &mut App, area: Rect) {
    if let Some(viewing) = app.viewing.clone() {
        // Stored-session viewer
        let inner_w = area.width.saturating_sub(4) as usize;
        let mut lines: Vec<Line> = Vec::new();
        let when = viewing
            .start_time
            .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default();
        let dur = format_secs(viewing.duration_secs);
        lines.push(Line::from(vec![
            Span::styled(
                if viewing.title.is_empty() {
                    viewing.name.clone()
                } else {
                    viewing.title.clone()
                },
                SPEAKER_STYLE,
            ),
            Span::styled(
                format!("  {when} • {dur} • {} words", viewing.word_count),
                DIM,
            ),
        ]));
        lines.push(Line::default());
        for raw in app.view_raw.lines() {
            if raw.is_empty() {
                lines.push(Line::default());
            } else {
                for l in word_wrap(raw, inner_w) {
                    lines.push(Line::raw(l));
                }
            }
        }
        let available = area.height.saturating_sub(2) as usize;

        let show_summary = !app.chat_open
            && app.summary_open
            && (!viewing.summary.is_empty() || app.summary_pending);

        if app.chat_open {
            let [left, right] =
                Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
                    .areas(area);
            let content = windowed(lines, left.height.saturating_sub(2) as usize, app.scroll);
            f.render_widget(
                Paragraph::new(content).block(Block::default().borders(Borders::ALL)),
                left,
            );
            draw_chat_pane(f, app, right);
        } else if show_summary {
            let [left, right] =
                Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)])
                    .areas(area);
            let content = windowed(lines, left.height.saturating_sub(2) as usize, app.scroll);
            f.render_widget(
                Paragraph::new(content).block(Block::default().borders(Borders::ALL)),
                left,
            );
            draw_summary_pane(f, app, &viewing.summary, right);
        } else {
            let content = windowed(lines, available, app.scroll);
            f.render_widget(
                Paragraph::new(content).block(Block::default().borders(Borders::ALL)),
                area,
            );
        }
        return;
    }

    // Session list
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled("Recordings", TITLE_STYLE)));
    lines.push(Line::default());
    if app.sessions.is_empty() {
        lines.push(Line::from(Span::styled(
            "No recordings yet. Press N to start one.",
            DIM,
        )));
    }
    for (i, s) in app.sessions.iter().enumerate() {
        let selected = i == app.session_idx;
        let display = if s.title.is_empty() { &s.name } else { &s.title };
        let display = truncate(display, 35);
        let when = s
            .start_time
            .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default();
        let meta = format!(
            "{when} • {} • {} words • {} speakers",
            format_secs(s.duration_secs),
            s.word_count,
            s.speaker_count
        );
        let (marker, name_style) = if selected {
            ("> ", Style::new().fg(Color::Blue).bold())
        } else {
            ("  ", Style::default())
        };
        lines.push(Line::from(vec![
            Span::raw(marker),
            Span::styled(display, name_style),
        ]));
        lines.push(Line::from(Span::styled(format!("    {meta}"), DIM)));
    }
    let block = Block::default().borders(Borders::ALL);
    if app.chat_open {
        let [left, right] =
            Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
                .areas(area);
        f.render_widget(Paragraph::new(lines).block(block), left);
        draw_chat_pane(f, app, right);
    } else {
        f.render_widget(Paragraph::new(lines).block(block), area);
    }
}

fn draw_mic_select(f: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        "Select microphone",
        TITLE_STYLE,
    )));
    lines.push(Line::default());
    for (i, d) in app.devices.iter().enumerate() {
        let selected = i == app.mic_idx;
        let (marker, style) = if selected {
            ("> ", Style::new().fg(Color::Blue).bold())
        } else {
            ("  ", Style::default())
        };
        lines.push(Line::from(Span::styled(
            format!("{marker}[{}] {}", d.index, d.name),
            style,
        )));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "↑/↓ Navigate • Enter Select • Esc Cancel",
        DIM,
    )));
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_settings(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(" Settings ", TITLE_STYLE));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let [tabs, body] = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(inner);
    let tab_line = Line::from(vec![
        Span::styled(if app.settings_tab == 0 { " Glossary " } else { " Glossary " }, if app.settings_tab == 0 { TITLE_STYLE } else { DIM }),
        Span::raw("  "),
        Span::styled(" STT Model ", if app.settings_tab == 1 { TITLE_STYLE } else { DIM }),
    ]);
    f.render_widget(Paragraph::new(tab_line), tabs);
    if app.settings_tab == 1 {
        let mut lines = vec![Line::from(Span::styled("Speech-to-text model for new recordings:", DIM))];
        for (i, p) in settings::stt_profiles().iter().enumerate() {
            let marker = if i == app.stt_idx { "▶" } else { " " };
            let availability = if p.local { if settings::stt_profile_available(*p) { "installed" } else { "not installed" } } else { "API" };
            lines.push(Line::from(format!("{marker} {:<40} [{}]", p.label, availability)));
        }
        lines.push(Line::from(Span::styled("Use ↑/↓, Enter to save; ←/→ or 1/2 switches tabs.", DIM)));
        f.render_widget(Paragraph::new(lines), body);
        return;
    }
    let [help, editor] = Layout::vertical([Constraint::Length(4), Constraint::Min(1)]).areas(body);

    let help_lines = vec![
        Line::from(Span::styled(
            "Corrects mis-heard words: fed to speech-to-text (keyterm biasing) and the AI.",
            DIM,
        )),
        Line::from(Span::styled(
            "One term or phrase per line — names, products, jargon.  # starts a comment.",
            DIM,
        )),
        Line::from(Span::styled(
            "Applies to the next recording.",
            DIM,
        )),
    ];
    f.render_widget(Paragraph::new(help_lines), help);
    f.render_widget(&app.settings_input, editor);
}

fn draw_status_bar(f: &mut Frame, app: &App, area: Rect) {
    let words = app
        .mgr
        .snapshot()
        .map(|s| (s.word_count, s.speaker_count))
        .unwrap_or((0, 0));
    let meter = level_meter(app.audio_level);
    let mut spans = vec![Span::raw(format!(
        " Duration: {} │ Words: {} │ Speakers: {} │ ",
        app.duration, words.0, words.1
    ))];
    spans.extend(meter);
    let bar_style = Style::new().bg(Color::Indexed(18));
    let line = Line::from(spans).style(bar_style);
    f.render_widget(Paragraph::new(line).style(bar_style), area);
}

fn level_meter(level: f64) -> Vec<Span<'static>> {
    let filled = (level * 8.0) as usize;
    (0..8)
        .map(|i| {
            if i < filled {
                let color = if i < 5 {
                    Color::Green
                } else if i < 7 {
                    Color::Yellow
                } else {
                    Color::Red
                };
                Span::styled("▄", Style::new().fg(color))
            } else {
                Span::styled("▁", DIM)
            }
        })
        .collect()
}

fn draw_controls(f: &mut Frame, app: &App, area: Rect) {
    if app.settings_open {
        let parts = ["[1/2] Tabs", "[Esc] Save & close", "[Ctrl+C] Cancel"];
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(parts.join(" │ "), DIM))),
            area,
        );
        return;
    }
    let mut parts: Vec<&str> = Vec::new();
    if app.home_mode {
        if app.viewing.is_some() {
            parts.push("[Esc] Back");
            parts.push("[N]ew Session");
            parts.push("[S]ummary");
            parts.push(if app.chat_open { "[Esc] Close Chat" } else { "[C]hat" });
        } else {
            let recording = matches!(app.status, Status::Recording | Status::Paused);
            if recording {
                parts.push("[Esc] Back to Recording");
            }
            parts.push("↑/↓ Navigate");
            parts.push("[Enter] View");
            if !recording {
                parts.push("[N]ew Session");
            }
            parts.push(if app.chat_open { "[Esc] Close Chat" } else { "[C]hat" });
            parts.push("[S]ettings");
            parts.push("[D]elete");
            parts.push("[Q]uit");
        }
    } else {
        match app.status {
            Status::Idle => parts.push("[N]ew Session"),
            Status::Finished => { parts.push("[Esc] Back"); parts.push("[N]ew Session"); }
            Status::Recording => {
                parts.push("[P]ause");
                parts.push("[F]inish");
                parts.push("[X] Discard");
            }
            Status::Paused => {
                parts.push("[P] Resume");
                parts.push("[F]inish");
                parts.push("[X] Discard");
            }
        }
        if app.status != Status::Recording {
            parts.push("[M]ic");
        }
        parts.push(if app.chat_open { "[Esc] Close Chat" } else { "[C]hat" });
        parts.push("[Q]uit");
    }
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(parts.join(" │ "), DIM))),
        area,
    );
}

fn draw_notice(f: &mut Frame, app: &App, area: Rect) {
    if app.notice.is_empty() {
        return;
    }
    let style = if app.notice_err { ERROR_STYLE } else { NOTICE_STYLE };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(app.notice.clone(), style))),
        area,
    );
}

fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width.saturating_sub(2));
    let h = height.min(area.height.saturating_sub(2));
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}

fn draw_template_modal(f: &mut Frame, app: &App, area: Rect) {
    let h = (app.templates.len() as u16) + 4;
    let rect = centered_rect(area, 44, h);
    f.render_widget(Clear, rect);
    let mut lines: Vec<Line> = Vec::new();
    for (i, t) in app.templates.iter().enumerate() {
        let selected = i == app.template_idx;
        let (marker, style) = if selected {
            ("> ", Style::new().fg(Color::Blue).bold())
        } else {
            ("  ", Style::default())
        };
        lines.push(Line::from(Span::styled(
            format!("{marker}{}", t.name),
            style,
        )));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled("Enter Ask • Esc Close", DIM)));
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Quick Questions ");
    f.render_widget(Paragraph::new(lines).block(block), rect);
}

fn draw_confirm_delete(f: &mut Frame, app: &App, area: Rect) {
    let name = app
        .sessions
        .get(app.session_idx)
        .map(|s| {
            if s.title.is_empty() {
                s.name.clone()
            } else {
                s.title.clone()
            }
        })
        .unwrap_or_default();
    let text = format!("Delete \u{201c}{}\u{201d}?", truncate(&name, 30));
    let rect = centered_rect(area, (text.chars().count() as u16 + 6).max(24), 5);
    f.render_widget(Clear, rect);
    let lines = vec![
        Line::from(Span::raw(text)),
        Line::default(),
        Line::from(vec![
            Span::styled("[Y]es", ERROR_STYLE),
            Span::raw(" / "),
            Span::styled("[N]o", Style::default()),
        ]),
    ];
    let block = Block::default().borders(Borders::ALL);
    f.render_widget(
        Paragraph::new(lines).block(block).centered(),
        rect,
    );
}

fn format_secs(secs: f64) -> String {
    let total = secs.max(0.0) as u64;
    format!("{:02}:{:02}:{:02}", total / 3600, (total % 3600) / 60, total % 60)
}
