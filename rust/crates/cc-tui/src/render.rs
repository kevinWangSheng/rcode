//! Ratatui rendering — pure function of `&App` plus a small theme.
//!
//! All layout and styling lives here. Keeping this file free of `Tokio` /
//! channels makes it easy to write golden-style tests with `TestBackend`.

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};

use crate::app::{App, AppMode, PendingPermission, TranscriptItem};
use crate::diff::render_unified_diff;
use crate::markdown::{minimal_mode_enabled, render_markdown};

const TITLE_USER: &str = ">";
const TITLE_CLAUDE: &str = "Claude:";
const TITLE_INFO: &str = "i";
const TITLE_BOUNDARY: &str = "-- compacted --";

pub fn render(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // status bar
            Constraint::Min(5),    // transcript
            Constraint::Length(3), // input box
        ])
        .split(frame.area());

    render_status_bar(frame, app, chunks[0]);
    render_transcript(frame, app, chunks[1]);
    render_input(frame, app, chunks[2]);

    if app.mode == AppMode::CommandPalette {
        render_command_palette(frame, app, chunks[2]);
    }

    if let Some(perm) = &app.permission {
        render_permission_modal(frame, perm, frame.area());
    }
}

fn render_command_palette(frame: &mut Frame, app: &App, input_area: Rect) {
    let n = app.palette_matches.len().min(8) as u16;
    if n == 0 {
        return;
    }
    let popup_height = n + 2; // +2 for the border.
    // Float the popup directly above the input box.
    let width = input_area.width.clamp(20, 50);
    let x = input_area.x;
    let y = input_area.y.saturating_sub(popup_height);
    let area = Rect {
        x,
        y,
        width,
        height: popup_height,
    };
    frame.render_widget(Clear, area);

    let mut rows: Vec<Line<'static>> = Vec::with_capacity(n as usize);
    for (i, name) in app.palette_matches.iter().take(8).enumerate() {
        let style = if i == app.palette_selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        rows.push(Line::from(Span::styled(format!(" /{name} "), style)));
    }

    let para = Paragraph::new(rows).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Commands ")
            .style(Style::default().fg(Color::Cyan)),
    );
    frame.render_widget(para, area);
}

fn render_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    // Build the status bar as a sequence of spans so the spinner glyph can
    // carry its own color (green while streaming) without re-styling the
    // whole line. The baseline line reads:
    //   ⠋ model: ... | tokens: in/out | $cost | turns: N [| (+N queued)] [| hint]
    let mut spans: Vec<Span<'static>> = Vec::new();
    let glyph = app.spinner_glyph();
    if !glyph.is_empty() {
        spans.push(Span::styled(
            format!("{glyph} "),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ));
    }
    spans.push(Span::styled(
        app.status.format(),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
    ));
    let queued = app.queued_count();
    if queued > 0 {
        spans.push(Span::styled(
            format!(" | (+{queued} queued)"),
            Style::default().fg(Color::Yellow),
        ));
    }
    if let Some(hint) = &app.status_hint {
        spans.push(Span::styled(
            format!(" | {hint}"),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        ));
    }
    let para = Paragraph::new(Line::from(spans));
    frame.render_widget(para, area);
}

fn render_transcript(frame: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    for item in &app.transcript {
        match item {
            TranscriptItem::UserMessage(text) => {
                let mut text_lines = text.lines();
                if let Some(first) = text_lines.next() {
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!("{TITLE_USER} "),
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(first.to_string()),
                    ]));
                } else {
                    lines.push(Line::from(Span::styled(
                        format!("{TITLE_USER} "),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    )));
                }
                for ln in text_lines {
                    lines.push(Line::from(Span::raw(format!("  {ln}"))));
                }
                lines.push(Line::from(""));
            }
            TranscriptItem::AssistantText(text) => {
                lines.push(Line::from(Span::styled(
                    TITLE_CLAUDE,
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                )));
                if minimal_mode_enabled() {
                    for ln in text.lines() {
                        lines.push(Line::from(Span::raw(ln.to_string())));
                    }
                } else {
                    lines.extend(render_markdown(text));
                }
                lines.push(Line::from(""));
            }
            TranscriptItem::ToolCall {
                name,
                input_summary,
                raw_input,
            } => {
                if minimal_mode_enabled() {
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!("[Tool: {name}] "),
                            Style::default()
                                .fg(Color::Magenta)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            input_summary.to_string(),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]));
                } else {
                    lines.push(render_tool_header(name, input_summary));
                    // Edit: show a unified diff of old_string → new_string if
                    // the raw input has both fields.
                    if name == "Edit" {
                        if let (Some(old), Some(new)) = (
                            raw_input
                                .get("old_string")
                                .and_then(|v| v.as_str()),
                            raw_input
                                .get("new_string")
                                .and_then(|v| v.as_str()),
                        ) {
                            for dline in render_unified_diff(old, new) {
                                let mut spans: Vec<Span<'static>> =
                                    vec![Span::raw("  ".to_string())];
                                spans.extend(dline.spans);
                                lines.push(Line::from(spans));
                            }
                        }
                    }
                }
            }
            TranscriptItem::ToolResult {
                name,
                output,
                is_error,
            } => {
                if minimal_mode_enabled() {
                    let color = if *is_error {
                        Color::Red
                    } else {
                        Color::DarkGray
                    };
                    let prefix = if *is_error { "[Error] " } else { "[Result] " };
                    let preview: String = output.lines().take(5).collect::<Vec<_>>().join("\n");
                    let truncated = output.lines().count() > 5;
                    lines.push(Line::from(Span::styled(
                        format!("{prefix}{preview}"),
                        Style::default().fg(color),
                    )));
                    if truncated {
                        lines.push(Line::from(Span::styled(
                            "  ... (truncated)",
                            Style::default().fg(Color::DarkGray),
                        )));
                    }
                    lines.push(Line::from(""));
                } else {
                    let mark = if *is_error { "✗" } else { "✓" };
                    let mark_color = if *is_error { Color::Red } else { Color::Green };
                    // First result line carries the tick; subsequent lines are
                    // indented so the card reads as a block even when the
                    // output is multi-line.
                    let mut iter = output.lines().take(5);
                    if let Some(first) = iter.next() {
                        lines.push(Line::from(vec![
                            Span::styled(
                                format!("  {mark} "),
                                Style::default()
                                    .fg(mark_color)
                                    .add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(first.to_string(), tool_output_style(name, *is_error)),
                        ]));
                    } else {
                        lines.push(Line::from(Span::styled(
                            format!("  {mark} (empty)"),
                            Style::default()
                                .fg(mark_color)
                                .add_modifier(Modifier::BOLD),
                        )));
                    }
                    for ln in iter {
                        lines.push(Line::from(vec![
                            Span::raw("    ".to_string()),
                            Span::styled(ln.to_string(), tool_output_style(name, *is_error)),
                        ]));
                    }
                    if output.lines().count() > 5 {
                        lines.push(Line::from(Span::styled(
                            "    … (truncated)".to_string(),
                            Style::default().fg(Color::DarkGray),
                        )));
                    }
                    lines.push(Line::from(""));
                }
            }
            TranscriptItem::SystemNotice(text) => {
                let mut text_lines = text.lines();
                if let Some(first) = text_lines.next() {
                    lines.push(Line::from(Span::styled(
                        format!("{TITLE_INFO} {first}"),
                        Style::default().fg(Color::Yellow),
                    )));
                } else {
                    lines.push(Line::from(Span::styled(
                        TITLE_INFO.to_string(),
                        Style::default().fg(Color::Yellow),
                    )));
                }
                for ln in text_lines {
                    lines.push(Line::from(Span::styled(
                        format!("  {ln}"),
                        Style::default().fg(Color::Yellow),
                    )));
                }
                lines.push(Line::from(""));
            }
            TranscriptItem::CompactBoundary => {
                lines.push(Line::from(Span::styled(
                    TITLE_BOUNDARY,
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::ITALIC),
                )));
                lines.push(Line::from(""));
            }
        }
    }

    if !app.streaming_text.is_empty() || app.mode == AppMode::Streaming {
        lines.push(Line::from(Span::styled(
            TITLE_CLAUDE,
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )));
        if minimal_mode_enabled() {
            for ln in app.streaming_text.lines() {
                lines.push(Line::from(Span::raw(ln.to_string())));
            }
        } else {
            lines.extend(render_markdown(&app.streaming_text));
        }
        if app.mode == AppMode::Streaming {
            lines.push(Line::from(Span::styled(
                "|",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::SLOW_BLINK),
            )));
        }
    }

    // Pin viewport to the bottom of the transcript by default.
    let wrap_width = (area.width.saturating_sub(2)) as usize;
    let total_rows: usize = if wrap_width == 0 {
        lines.len()
    } else {
        lines
            .iter()
            .map(|line| {
                let char_count: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
                char_count.div_ceil(wrap_width).max(1)
            })
            .sum()
    };
    let viewport_rows = area.height.saturating_sub(2) as usize;
    let max_scroll = total_rows.saturating_sub(viewport_rows) as u16;
    let y_scroll = max_scroll.saturating_sub(app.scroll);

    let title = format!(" Claude -- session {} ", short_session(&app.session_id));
    let para = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: false })
        .scroll((y_scroll, 0));
    frame.render_widget(para, area);
}

fn render_input(frame: &mut Frame, app: &App, area: Rect) {
    let title = match app.mode {
        AppMode::Input => " Input  (Enter to send  |  / for commands) ",
        AppMode::Streaming => " Input  (streaming -- Enter queues  |  Ctrl+C aborts) ",
        AppMode::PermissionPrompt => " Input  (permission prompt -- please respond) ",
        AppMode::CommandPalette => " Input  (/ command autocomplete) ",
    };
    let style = match app.mode {
        AppMode::Input | AppMode::CommandPalette => Style::default().fg(Color::White),
        AppMode::Streaming => Style::default().fg(Color::Yellow),
        AppMode::PermissionPrompt => Style::default().fg(Color::DarkGray),
    };
    let para = Paragraph::new(app.input.as_str())
        .style(style)
        .block(Block::default().borders(Borders::ALL).title(title));
    frame.render_widget(para, area);
}

fn render_permission_modal(frame: &mut Frame, perm: &PendingPermission, area: Rect) {
    let modal = centered_rect(60, 30, area);
    frame.render_widget(Clear, modal);

    let body = vec![
        Line::from(Span::styled(
            format!("Tool: {}", perm.tool_name),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(perm.summary.as_str()),
        Line::from(""),
        Line::from(Span::styled(
            "[y] Allow once    [a] Always allow    [n / Esc] Reject",
            Style::default().fg(Color::White),
        )),
    ];

    let para = Paragraph::new(body)
        .alignment(Alignment::Left)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Permission required ")
                .style(Style::default().fg(Color::Yellow)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(para, modal);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

/// Color assigned to a tool card based on the tool name. Matches M5 scope:
/// Bash=green, Edit=yellow, Read=blue, Grep/Glob=cyan, Web*=magenta, MCP=
/// dim-white (DarkGray), fallback=magenta to stay visible.
fn tool_color(name: &str) -> Color {
    match name {
        "Bash" => Color::Green,
        "Edit" | "Write" => Color::Yellow,
        "Read" => Color::Blue,
        "Grep" | "Glob" => Color::Cyan,
        "WebFetch" | "WebSearch" => Color::Magenta,
        n if n.contains("::") => Color::Gray, // MCP server::tool
        _ => Color::Magenta,
    }
}

fn tool_output_style(name: &str, is_error: bool) -> Style {
    if is_error {
        Style::default().fg(Color::Red)
    } else {
        // Neutral text for the body; the header carries the tool color.
        let _ = tool_color(name);
        Style::default().fg(Color::White)
    }
}

fn render_tool_header(name: &str, preview: &str) -> Line<'static> {
    // `⏺ ToolName(preview_args)` per M5 Phase B.
    let color = tool_color(name);
    let preview = preview.lines().next().unwrap_or("").to_string();
    Line::from(vec![
        Span::styled(
            "⏺ ".to_string(),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            name.to_string(),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled("(".to_string(), Style::default().fg(Color::DarkGray)),
        Span::styled(preview, Style::default().fg(Color::White)),
        Span::styled(")".to_string(), Style::default().fg(Color::DarkGray)),
    ])
}

fn short_session(id: &str) -> &str {
    if id.len() > 8 {
        &id[..8]
    } else {
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use crate::app::{App, PendingPermission};

    #[test]
    fn renders_empty_app_without_panic() {
        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        let app = App::new("abcdef1234".into(), "test-model".into());
        term.draw(|f| render(f, &app)).unwrap();
        let buf = term.backend().buffer().clone();
        let s = buffer_to_string(&buf);
        assert!(
            s.contains("abcdef12"),
            "title bar missing session id; got:\n{s}"
        );
    }

    #[test]
    fn renders_streaming_text_and_cursor() {
        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        let mut app = App::new("s".into(), "m".into());
        app.start_stream();
        app.on_token("hello world");
        term.draw(|f| render(f, &app)).unwrap();
        let s = buffer_to_string(term.backend().buffer());
        assert!(s.contains("hello world"));
    }

    #[test]
    fn multiline_system_message_renders_on_separate_rows() {
        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        let mut app = App::new("s".into(), "m".into());
        app.push_system("Slash commands:\n  /help   show this help\n  /exit   quit".into());
        term.draw(|f| render(f, &app)).unwrap();
        let s = buffer_to_string(term.backend().buffer());
        assert!(s.contains("Slash commands:"));
        assert!(s.contains("/help"));
        assert!(s.contains("/exit"));
    }

    #[test]
    fn long_transcript_pins_latest_content_to_bottom() {
        let backend = TestBackend::new(80, 10);
        let mut term = Terminal::new(backend).unwrap();
        let mut app = App::new("s".into(), "m".into());
        for i in 0..40 {
            app.push_user(format!("user message number {i}"));
        }
        app.scroll = 0;
        term.draw(|f| render(f, &app)).unwrap();
        let s = buffer_to_string(term.backend().buffer());
        assert!(
            s.contains("user message number 39"),
            "latest message should be visible at the bottom; got:\n{s}"
        );
    }

    #[test]
    fn renders_permission_modal_when_set() {
        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        let mut app = App::new("s".into(), "m".into());
        app.permission = Some(PendingPermission {
            tool_name: "Write".into(),
            summary: "/tmp/foo.txt".into(),
        });
        term.draw(|f| render(f, &app)).unwrap();
        let s = buffer_to_string(term.backend().buffer());
        assert!(s.contains("Permission required"));
        assert!(s.contains("Write"));
    }

    #[test]
    fn renders_tool_call_and_result() {
        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        let mut app = App::new("s".into(), "m".into());
        app.push_tool_call("Bash".into(), "ls -la".into());
        app.push_tool_result("Bash".into(), "file1.rs\nfile2.rs".into(), false);
        term.draw(|f| render(f, &app)).unwrap();
        let s = buffer_to_string(term.backend().buffer());
        assert!(s.contains("Bash"));
        assert!(s.contains("ls -la"));
    }

    fn buffer_to_string(buf: &ratatui::buffer::Buffer) -> String {
        let mut s = String::new();
        let area = buf.area;
        for y in 0..area.height {
            for x in 0..area.width {
                s.push_str(buf[(x, y)].symbol());
            }
            s.push('\n');
        }
        s
    }
}
