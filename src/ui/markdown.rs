//! Minimal markdown → ratatui rendering for chat replies (the subset glamour
//! actually showed: headings, bold/italic, inline code, code blocks, bullets).

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::wrap::word_wrap;

pub fn render_markdown(text: &str, width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();
    let mut current: Vec<Span> = Vec::new();
    let mut style_stack: Vec<Style> = vec![Style::default()];
    let mut in_code_block = false;
    let mut list_depth: usize = 0;

    let flush = |current: &mut Vec<Span<'static>>, lines: &mut Vec<Line<'static>>| {
        if !current.is_empty() {
            lines.push(Line::from(std::mem::take(current)));
        }
    };

    let parser = Parser::new_ext(text, Options::empty());
    for event in parser {
        let style = *style_stack.last().unwrap();
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                flush(&mut current, &mut lines);
                if !lines.is_empty() {
                    lines.push(Line::default());
                }
                let s = Style::default().add_modifier(Modifier::BOLD).fg(match level {
                    HeadingLevel::H1 => Color::Magenta,
                    _ => Color::Blue,
                });
                style_stack.push(s);
            }
            Event::End(TagEnd::Heading(_)) => {
                style_stack.pop();
                flush(&mut current, &mut lines);
            }
            Event::Start(Tag::Strong) => {
                style_stack.push(style.add_modifier(Modifier::BOLD));
            }
            Event::End(TagEnd::Strong) => {
                style_stack.pop();
            }
            Event::Start(Tag::Emphasis) => {
                style_stack.push(style.add_modifier(Modifier::ITALIC));
            }
            Event::End(TagEnd::Emphasis) => {
                style_stack.pop();
            }
            Event::Start(Tag::CodeBlock(_)) => {
                flush(&mut current, &mut lines);
                in_code_block = true;
            }
            Event::End(TagEnd::CodeBlock) => {
                in_code_block = false;
            }
            Event::Start(Tag::List(_)) => {
                flush(&mut current, &mut lines);
                list_depth += 1;
            }
            Event::End(TagEnd::List(_)) => {
                list_depth = list_depth.saturating_sub(1);
            }
            Event::Start(Tag::Item) => {
                flush(&mut current, &mut lines);
                current.push(Span::raw(format!("{}• ", "  ".repeat(list_depth.saturating_sub(1)))));
            }
            Event::End(TagEnd::Item) => {
                flush(&mut current, &mut lines);
            }
            Event::Start(Tag::Paragraph) => {
                flush(&mut current, &mut lines);
                if !lines.is_empty() && list_depth == 0 {
                    lines.push(Line::default());
                }
            }
            Event::End(TagEnd::Paragraph) => {
                flush(&mut current, &mut lines);
            }
            Event::Text(t) => {
                if in_code_block {
                    for l in t.lines() {
                        lines.push(Line::from(Span::styled(
                            format!("  {l}"),
                            Style::default().fg(Color::Yellow),
                        )));
                    }
                } else {
                    // Wrap long runs of text at the pane width.
                    let prefix_w: usize = current
                        .iter()
                        .map(|s| unicode_width::UnicodeWidthStr::width(s.content.as_ref()))
                        .sum();
                    let wrapped = word_wrap(&t, width.max(prefix_w + 4) - prefix_w.min(width));
                    for (i, piece) in wrapped.iter().enumerate() {
                        if i > 0 {
                            flush(&mut current, &mut lines);
                        }
                        current.push(Span::styled(piece.clone(), style));
                    }
                }
            }
            Event::Code(code) => {
                current.push(Span::styled(
                    code.to_string(),
                    Style::default().fg(Color::Yellow),
                ));
            }
            Event::SoftBreak => {
                current.push(Span::raw(" "));
            }
            Event::HardBreak => {
                flush(&mut current, &mut lines);
            }
            _ => {}
        }
    }
    flush(&mut current, &mut lines);
    lines
}
