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

use crate::app::{App, PendingPermission, StreamState, TranscriptItem};

const TITLE_USER: &str = "▶";
const TITLE_CLAUDE: &str = "Claude:";
const TITLE_INFO: &str = "ℹ";
const TITLE_BOUNDARY: &str = "── compacted ──";

pub fn render(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(5),    // transcript
            Constraint::Length(3), // input box
            Constraint::Length(1), // status line
        ])
        .split(frame.area());

    render_transcript(frame, app, chunks[0]);
    render_input(frame, app, chunks[1]);
    render_status(frame, app, chunks[2]);

    if let Some(perm) = &app.permission {
        render_permission_modal(frame, perm, frame.area());
    }
}

fn render_transcript(frame: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    for item in &app.transcript {
        match item {
            TranscriptItem::User(text) => {
                let mut text_lines = text.lines();
                // First physical line gets the "▶ " prefix; subsequent lines are
                // indented to visually continue the user turn.
                if let Some(first) = text_lines.next() {
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!("{TITLE_USER} "),
                            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(first.to_string()),
                    ]));
                } else {
                    lines.push(Line::from(Span::styled(
                        format!("{TITLE_USER} "),
                        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                    )));
                }
                for ln in text_lines {
                    lines.push(Line::from(Span::raw(format!("  {ln}"))));
                }
                lines.push(Line::from(""));
            }
            TranscriptItem::Assistant(text) => {
                lines.push(Line::from(Span::styled(
                    TITLE_CLAUDE,
                    Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                )));
                for ln in text.lines() {
                    lines.push(Line::from(Span::raw(ln.to_string())));
                }
                lines.push(Line::from(""));
            }
            TranscriptItem::System(text) => {
                // System messages are often multi-line (/help output, engine
                // errors). Prefix only the first physical line; indent the
                // rest to line up under the icon.
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
                    Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
                )));
                lines.push(Line::from(""));
            }
        }
    }

    if !app.streaming_text.is_empty() || app.stream_state == StreamState::Streaming {
        lines.push(Line::from(Span::styled(
            TITLE_CLAUDE,
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        )));
        for ln in app.streaming_text.lines() {
            lines.push(Line::from(Span::raw(ln.to_string())));
        }
        if app.stream_state == StreamState::Streaming {
            lines.push(Line::from(Span::styled(
                "▋",
                Style::default().fg(Color::Green).add_modifier(Modifier::SLOW_BLINK),
            )));
        }
    }

    // Pin viewport to the *bottom* of the transcript by default, so the
    // latest content is always visible as history grows. `app.scroll` is
    // treated as "rows back from the bottom" (0 = pinned to bottom).
    //
    // We estimate the post-wrap physical row count by accounting for lines
    // that exceed the inner width of the Block (area.width minus 2 for the
    // borders). Under-counting for exotic Unicode is acceptable — the worst
    // case is a few extra rows visible at the bottom on very wide chars.
    let wrap_width = (area.width.saturating_sub(2)) as usize;
    let total_rows: usize = if wrap_width == 0 {
        lines.len()
    } else {
        lines
            .iter()
            .map(|line| {
                let char_count: usize =
                    line.spans.iter().map(|s| s.content.chars().count()).sum();
                // Empty logical lines still occupy one physical row.
                char_count.div_ceil(wrap_width).max(1)
            })
            .sum()
    };
    let viewport_rows = area.height.saturating_sub(2) as usize; // minus top+bottom borders
    let max_scroll = total_rows.saturating_sub(viewport_rows) as u16;
    let y_scroll = max_scroll.saturating_sub(app.scroll);

    let title = format!(" Claude — session {} ", short_session(&app.session_id));
    let para = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: false })
        .scroll((y_scroll, 0));
    frame.render_widget(para, area);
}

fn render_input(frame: &mut Frame, app: &App, area: Rect) {
    let title = match app.stream_state {
        StreamState::Idle => " Input  (Enter to send  •  / for commands) ",
        StreamState::Streaming => " Input  (streaming — Enter queues  •  Ctrl+C aborts) ",
        StreamState::ToolUse => " Input  (tool running — please wait) ",
    };
    let style = match app.stream_state {
        StreamState::Idle => Style::default().fg(Color::White),
        StreamState::Streaming => Style::default().fg(Color::Yellow),
        StreamState::ToolUse => Style::default().fg(Color::DarkGray),
    };
    let para = Paragraph::new(app.input.as_str())
        .style(style)
        .block(Block::default().borders(Borders::ALL).title(title));
    frame.render_widget(para, area);
}

fn render_status(frame: &mut Frame, app: &App, area: Rect) {
    let para = Paragraph::new(app.status.as_str())
        .style(Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC));
    frame.render_widget(para, area);
}

fn render_permission_modal(frame: &mut Frame, perm: &PendingPermission, area: Rect) {
    // Centered modal sized to ~60% of width and a fixed height.
    let modal = centered_rect(60, 30, area);
    frame.render_widget(Clear, modal);

    let body = vec![
        Line::from(Span::styled(
            format!("Tool: {}", perm.tool_name),
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
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
        let app = App::new("abcdef1234".into());
        term.draw(|f| render(f, &app)).unwrap();
        let buf = term.backend().buffer().clone();
        // Title bar should mention the truncated session id.
        let s = buffer_to_string(&buf);
        assert!(s.contains("abcdef12"), "title bar missing session id; got:\n{s}");
    }

    #[test]
    fn renders_streaming_text_and_cursor() {
        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        let mut app = App::new("s".into());
        app.start_stream();
        app.on_token("hello world");
        term.draw(|f| render(f, &app)).unwrap();
        let s = buffer_to_string(term.backend().buffer());
        assert!(s.contains("hello world"));
    }

    #[test]
    fn multiline_system_message_renders_on_separate_rows() {
        // Regression: /help and other multi-line system messages used to be
        // squashed onto a single logical `Line`, so embedded '\n' chars were
        // mashed into one long wrap-blob. Verify each line gets its own row.
        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        let mut app = App::new("s".into());
        app.push_system("Slash commands:\n  /help   show this help\n  /exit   quit".into());
        term.draw(|f| render(f, &app)).unwrap();
        let s = buffer_to_string(term.backend().buffer());
        // Each segment should appear on its own row, not mashed on one line.
        assert!(s.contains("Slash commands:"));
        assert!(s.contains("/help"));
        assert!(s.contains("/exit"));
        // "show this help" must NOT be on the same row as "quit" — check by
        // finding rows containing each and asserting different row indices.
        let rows: Vec<&str> = s.lines().collect();
        let help_row = rows.iter().position(|r| r.contains("show this help"));
        let exit_row = rows.iter().position(|r| r.contains("quit"));
        assert!(help_row.is_some() && exit_row.is_some());
        assert_ne!(
            help_row, exit_row,
            "/help description and /exit must render on different rows"
        );
    }

    #[test]
    fn long_transcript_pins_latest_content_to_bottom() {
        // Regression: when transcript height exceeds the viewport, older
        // content used to stay at the top and the newest line was clipped
        // behind the input box. Verify the latest user message is visible.
        let backend = TestBackend::new(80, 10);
        let mut term = Terminal::new(backend).unwrap();
        let mut app = App::new("s".into());
        for i in 0..40 {
            app.push_user(format!("user message number {i}"));
        }
        // Make sure the "latest first" flag is off (scroll=0 == pinned to bottom).
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
        let mut app = App::new("s".into());
        app.permission = Some(PendingPermission {
            tool_name: "Write".into(),
            summary: "/tmp/foo.txt".into(),
        });
        term.draw(|f| render(f, &app)).unwrap();
        let s = buffer_to_string(term.backend().buffer());
        assert!(s.contains("Permission required"));
        assert!(s.contains("Write"));
        assert!(s.contains("/tmp/foo.txt"));
        assert!(s.contains("Allow once"));
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
