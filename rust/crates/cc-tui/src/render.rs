//! Ratatui rendering — pure function of `&App` plus the shared theme.
//!
//! Phase D layout (top → bottom):
//!
//! ```text
//! ┌──────────────────────────────────────────────────────┐
//! │ transcript (Min(0), unbordered, wraps, pinned bottom)│
//! ├──────────────────────────────────────────────────────┤
//! │ spinner row (Length 1) — verb + elapsed/tokens/cost  │
//! ├──────────────────────────────────────────────────────┤
//! │ PromptInput (Length 3) — bordered, dynamic colour    │
//! ├──────────────────────────────────────────────────────┤
//! │ help footer (Length 1) — mode-specific hints         │
//! ├──────────────────────────────────────────────────────┤
//! │ status bar (Length 1) — model · ctx · session · git  │
//! └──────────────────────────────────────────────────────┘
//! ```
//!
//! Keeping this file free of `Tokio` / channels makes it easy to write
//! golden-style tests with `TestBackend`.

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, AppMode, PendingPermission, TranscriptItem};
use crate::diff::render_unified_diff;
use crate::markdown::{minimal_mode_enabled, render_markdown};
use crate::theme::{self, Theme};
use crate::welcome;

/// Tool-card bullet glyph. On macOS we use `⏺` (U+23FA) — the same glyph
/// `figures.ts` picks for Darwin. On Linux / Windows many default monospace
/// fonts render U+23FA as a tofu box, so we fall back to `●` (U+25CF),
/// which ships with every reasonable console font. Matches the original
/// `figures.BLACK_CIRCLE` per-platform table in `src/constants/figures.ts`.
#[cfg(target_os = "macos")]
const TOOL_BULLET: &str = "⏺";
#[cfg(not(target_os = "macos"))]
const TOOL_BULLET: &str = "●";

/// Total context-window size used as the denominator for the "context used"
/// percentage in the status bar. Aligned with `cc_core::model::DEFAULT_CONTEXT_WINDOW`
/// (200K, which matches the TS original's `MODEL_CONTEXT_WINDOW_DEFAULT` and
/// every other 200K reference in the workspace). The auto-compact threshold
/// (~180K) is enforced separately in `cc-query/src/engine.rs`, so the
/// percentage shown here reflects the true window, not the trigger.
const CONTEXT_TOKEN_BUDGET: u64 = cc_core::model::models::DEFAULT_CONTEXT_WINDOW as u64;

pub fn render(frame: &mut Frame, app: &App) {
    let theme = theme::current();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),    // transcript
            Constraint::Length(1), // spinner row
            Constraint::Length(3), // input box
            Constraint::Length(1), // help footer
            Constraint::Length(1), // status bar
        ])
        .split(frame.area());

    render_transcript(frame, app, chunks[0], &theme);
    render_spinner_row(frame, app, chunks[1], &theme);
    render_input(frame, app, chunks[2], &theme);
    render_help_footer(frame, app, chunks[3], &theme);
    render_status_bar(frame, app, chunks[4], &theme);

    if app.mode == AppMode::CommandPalette {
        render_command_palette(frame, app, chunks[2], &theme);
    }

    if let Some(perm) = &app.permission {
        render_permission_modal(frame, perm, frame.area(), &theme);
    }
}

// ─── transcript ────────────────────────────────────────────────────────────

fn render_transcript(frame: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    let mut lines: Vec<Line<'static>> = Vec::new();

    // Empty session → render the welcome banner instead. Covers AC-V10.
    // CC_TUI_MINIMAL skips the banner so scripted captures stay reproducible.
    if app.is_empty_session() && !minimal_mode_enabled() {
        let tip_seed = app.session_started.elapsed().as_secs() / 30;
        lines.extend(welcome::render_welcome(
            area.width,
            &app.version,
            &app.cwd,
            tip_seed,
        ));
    }

    // Skip items already flushed to terminal scrollback via `insert_before`.
    // `run_tui` keeps `emitted_to_scrollback` monotonically increasing; this
    // slice is "finalized but not-yet-flushed" items landed between two
    // frames, which still need to be drawn this frame until the post-draw
    // flush catches up.
    let start = app.emitted_to_scrollback.min(app.transcript.len());
    for item in &app.transcript[start..] {
        push_transcript_item(&mut lines, item, area.width, theme);
    }

    if !app.streaming_text.is_empty() || app.mode == AppMode::Streaming {
        // Streaming text flows bare — no `Claude:` header per Phase D5.
        if minimal_mode_enabled() {
            for ln in app.streaming_text.lines() {
                lines.push(Line::from(Span::raw(ln.to_string())));
            }
        } else {
            lines.extend(render_markdown(&app.streaming_text));
        }
        if app.mode == AppMode::Streaming {
            // Caret blink so users see the stream is alive even between
            // token deltas.
            lines.push(Line::from(Span::styled(
                "▌",
                Style::default()
                    .fg(theme.claude_orange)
                    .add_modifier(Modifier::SLOW_BLINK),
            )));
        }
    }

    // Pin viewport to the bottom of the transcript by default.
    //
    // Count *terminal cells*, not Unicode code points. CJK ideographs and
    // emoji take two cells while `chars().count()` returns one, so using
    // the raw char count made the pin-to-bottom drift upward by up to half
    // the visible rows on Chinese-heavy transcripts (BUG-2 / BUG-3).
    // `unicode-width` gives the same width Ratatui's internal word-wrap
    // uses, so the row count stays consistent with what actually renders.
    let wrap_width = area.width.max(1) as usize;
    let total_rows: usize = lines
        .iter()
        .map(|line| {
            let cells: usize = line
                .spans
                .iter()
                .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
                .sum();
            cells.div_ceil(wrap_width.max(1)).max(1)
        })
        .sum();
    let viewport_rows = area.height as usize;
    let max_scroll = total_rows.saturating_sub(viewport_rows) as u16;
    let y_scroll = max_scroll.saturating_sub(app.scroll);

    let para = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((y_scroll, 0));
    frame.render_widget(para, area);
}

/// Render a single transcript item into its row lines. Used both by the
/// in-viewport render path (for items not yet flushed) and by
/// `run_tui`'s `insert_before` path (for items being pushed into the
/// terminal scrollback so the user can scroll through history with
/// their normal terminal controls).
pub fn render_item_lines(
    item: &TranscriptItem,
    viewport_width: u16,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    push_transcript_item(&mut lines, item, viewport_width, theme);
    lines
}

fn push_transcript_item(
    lines: &mut Vec<Line<'static>>,
    item: &TranscriptItem,
    width: u16,
    theme: &Theme,
) {
    match item {
        TranscriptItem::UserMessage(text) => push_user_message(lines, text, theme),
        TranscriptItem::AssistantText(text) => push_assistant_text(lines, text),
        TranscriptItem::ToolCall {
            name,
            input_summary,
            raw_input,
        } => push_tool_call(lines, name, input_summary, raw_input, theme),
        TranscriptItem::ToolResult {
            name,
            output,
            is_error,
        } => push_tool_result(lines, name, output, *is_error, theme),
        TranscriptItem::SystemNotice(text) => push_system_notice(lines, text, theme),
        TranscriptItem::CompactBoundary => push_compact_boundary(lines, width, theme),
    }
}

// Phase D5 gutter conventions:
//   user      → `>` claude-orange + text, intra-message indent of 2
//   assistant → no prefix; markdown content flows bare
//   tool      → `⏺` + tool-color name + (preview) + trailing tick
//   system    → `ⓘ` warning-yellow + text
//   compact   → centered `── compacted ──` dim italic
fn push_user_message(lines: &mut Vec<Line<'static>>, text: &str, theme: &Theme) {
    let mut text_lines = text.lines();
    if let Some(first) = text_lines.next() {
        lines.push(Line::from(vec![
            Span::styled(
                "> ".to_string(),
                Style::default()
                    .fg(theme.claude_orange)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(first.to_string()),
        ]));
    } else {
        lines.push(Line::from(Span::styled(
            "> ".to_string(),
            Style::default()
                .fg(theme.claude_orange)
                .add_modifier(Modifier::BOLD),
        )));
    }
    for ln in text_lines {
        lines.push(Line::from(Span::raw(format!("  {ln}"))));
    }
    lines.push(Line::from(""));
}

fn push_assistant_text(lines: &mut Vec<Line<'static>>, text: &str) {
    if minimal_mode_enabled() {
        for ln in text.lines() {
            lines.push(Line::from(Span::raw(ln.to_string())));
        }
    } else {
        lines.extend(render_markdown(text));
    }
    lines.push(Line::from(""));
}

fn push_tool_call(
    lines: &mut Vec<Line<'static>>,
    name: &str,
    input_summary: &str,
    raw_input: &serde_json::Value,
    theme: &Theme,
) {
    if minimal_mode_enabled() {
        lines.push(Line::from(vec![
            Span::styled(
                format!("[Tool: {name}] "),
                Style::default()
                    .fg(theme.tool_color(name))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(input_summary.to_string(), Style::default().fg(theme.dim)),
        ]));
        return;
    }

    lines.push(render_tool_header(name, input_summary, theme));

    // Edit: render unified diff if the raw input has both old/new strings.
    if matches!(name, "Edit" | "MultiEdit") {
        if let (Some(old), Some(new)) = (
            raw_input.get("old_string").and_then(|v| v.as_str()),
            raw_input.get("new_string").and_then(|v| v.as_str()),
        ) {
            for dline in render_unified_diff(old, new) {
                let mut spans: Vec<Span<'static>> = vec![Span::raw("  ".to_string())];
                spans.extend(dline.spans);
                lines.push(Line::from(spans));
            }
        }
    }
}

fn push_tool_result(
    lines: &mut Vec<Line<'static>>,
    name: &str,
    output: &str,
    is_error: bool,
    theme: &Theme,
) {
    if minimal_mode_enabled() {
        let color = if is_error { theme.error } else { theme.dim };
        let prefix = if is_error { "[Error] " } else { "[Result] " };
        let preview: String = output.lines().take(5).collect::<Vec<_>>().join("\n");
        let truncated = output.lines().count() > 5;
        lines.push(Line::from(Span::styled(
            format!("{prefix}{preview}"),
            Style::default().fg(color),
        )));
        if truncated {
            lines.push(Line::from(Span::styled(
                "  ... (truncated)",
                Style::default().fg(theme.dim),
            )));
        }
        lines.push(Line::from(""));
        return;
    }

    let mark = if is_error { "✗" } else { "✓" };
    let mark_color = if is_error { theme.error } else { theme.success };

    // First line carries the tick + prefix; subsequent lines indent so the
    // card reads as a block even when the output is multi-line.
    let mut iter = output.lines().take(5);
    if let Some(first) = iter.next() {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {mark} "),
                Style::default().fg(mark_color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(first.to_string(), tool_output_style(theme, is_error)),
        ]));
    } else {
        lines.push(Line::from(Span::styled(
            format!("  {mark} (empty)"),
            Style::default().fg(mark_color).add_modifier(Modifier::BOLD),
        )));
    }
    for ln in iter {
        lines.push(Line::from(vec![
            Span::raw("    ".to_string()),
            Span::styled(ln.to_string(), tool_output_style(theme, is_error)),
        ]));
    }
    if output.lines().count() > 5 {
        lines.push(Line::from(Span::styled(
            "    … (truncated)".to_string(),
            Style::default().fg(theme.dim),
        )));
    }
    let _ = name; // tool_color(name) currently feeds the header; body stays neutral.
    lines.push(Line::from(""));
}

fn push_system_notice(lines: &mut Vec<Line<'static>>, text: &str, theme: &Theme) {
    let mut text_lines = text.lines();
    if let Some(first) = text_lines.next() {
        lines.push(Line::from(vec![
            Span::styled(
                "ⓘ  ".to_string(),
                Style::default()
                    .fg(theme.warning)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(first.to_string(), Style::default().fg(theme.warning)),
        ]));
    } else {
        lines.push(Line::from(Span::styled(
            "ⓘ".to_string(),
            Style::default().fg(theme.warning),
        )));
    }
    for ln in text_lines {
        lines.push(Line::from(Span::styled(
            format!("   {ln}"),
            Style::default().fg(theme.warning),
        )));
    }
    lines.push(Line::from(""));
}

fn push_compact_boundary(lines: &mut Vec<Line<'static>>, width: u16, theme: &Theme) {
    // Match the TS original (`CompactBoundaryMessage.tsx`): a left-aligned
    // `✻` sparkle followed by the plain label plus a shortcut hint, all
    // dim. No centering, no rule run — a screen-width rule is noisy when
    // the transcript is already heavy with tool cards.
    let _ = width;
    lines.push(Line::from(vec![
        Span::styled(
            "✻ ".to_string(),
            Style::default()
                .fg(theme.claude_orange)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "Conversation compacted ".to_string(),
            Style::default().fg(theme.dim),
        ),
        Span::styled(
            "(ctrl+o for history)".to_string(),
            Style::default()
                .fg(theme.subtle)
                .add_modifier(Modifier::ITALIC),
        ),
    ]));
    lines.push(Line::from(""));
}

fn tool_output_style(theme: &Theme, is_error: bool) -> Style {
    if is_error {
        Style::default().fg(theme.error)
    } else {
        Style::default().fg(theme.text)
    }
}

fn render_tool_header(name: &str, preview: &str, theme: &Theme) -> Line<'static> {
    // `⏺ ToolName(preview_args)` per M5 Phase B + theme colours per D1.
    // Bullet glyph is platform-aware (see `TOOL_BULLET`).
    let color = theme.tool_color(name);
    let preview = preview.lines().next().unwrap_or("").to_string();
    Line::from(vec![
        Span::styled(
            format!("{TOOL_BULLET} "),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            name.to_string(),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled("(".to_string(), Style::default().fg(theme.subtle)),
        Span::styled(preview, Style::default().fg(theme.text)),
        Span::styled(")".to_string(), Style::default().fg(theme.subtle)),
    ])
}

// ─── spinner row (D6) ──────────────────────────────────────────────────────

fn render_spinner_row(frame: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    if app.stream_started_at.is_none() {
        // Idle — keep the row blank so the layout stays stable but the
        // user sees no flicker between turns.
        return;
    }

    let glyph = app.spinner_glyph();
    let verb = app.spinner_verb().unwrap_or("Working…");
    let elapsed = app.turn_elapsed_secs().unwrap_or(0);
    let cost = app.status.estimated_cost_usd;
    let in_t = app.status.input_tokens;
    let out_t = app.status.output_tokens;

    let mut spans: Vec<Span<'static>> = Vec::with_capacity(6);
    spans.push(Span::styled(
        format!("{glyph} "),
        Style::default()
            .fg(theme.claude_orange)
            .add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled(
        verb.to_string(),
        Style::default()
            .fg(theme.claude_orange)
            .add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled(
        format!("   {elapsed}s"),
        Style::default().fg(theme.dim),
    ));
    spans.push(Span::styled(
        format!(
            " · ↑ {} ↓ {}",
            crate::app::format_tokens_pub(in_t),
            crate::app::format_tokens_pub(out_t),
        ),
        Style::default().fg(theme.dim),
    ));
    spans.push(Span::styled(
        format!(" · ${cost:.4}"),
        Style::default().fg(theme.dim),
    ));
    if app.queued_count() > 0 {
        spans.push(Span::styled(
            format!("   (+{} queued)", app.queued_count()),
            Style::default().fg(theme.warning),
        ));
    }

    let para = Paragraph::new(Line::from(spans));
    frame.render_widget(para, area);
}

// ─── input box (D4) ────────────────────────────────────────────────────────

fn render_input(frame: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    let border_color = match app.mode {
        AppMode::Input | AppMode::CommandPalette => theme.dim,
        AppMode::Streaming => theme.claude_orange,
        AppMode::PermissionPrompt => theme.permission_blue,
    };

    // Inner content: gutter glyph + buffer (or placeholder when idle/empty).
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(2);
    spans.push(Span::styled(
        "> ".to_string(),
        Style::default()
            .fg(theme.claude_orange)
            .add_modifier(Modifier::BOLD),
    ));
    if app.input.is_empty() && app.mode == AppMode::Input {
        spans.push(Span::styled(
            "Ask Claude…".to_string(),
            Style::default()
                .fg(theme.dim)
                .add_modifier(Modifier::ITALIC),
        ));
    } else {
        spans.push(Span::styled(
            app.input.clone(),
            Style::default().fg(theme.text),
        ));
    }

    let para = Paragraph::new(Line::from(spans)).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color)),
    );
    frame.render_widget(para, area);
}

// ─── help footer (D4) ──────────────────────────────────────────────────────

fn render_help_footer(frame: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    let text = match app.mode {
        AppMode::Input => "? for shortcuts  ·  / for commands  ·  @ for files  ·  ! for bash",
        AppMode::Streaming => "esc to interrupt  ·  ctrl+c to cancel",
        AppMode::PermissionPrompt => "y allow  ·  a always  ·  n deny",
        AppMode::CommandPalette => "↑↓ select  ·  tab/enter accept  ·  esc cancel",
    };
    let para = Paragraph::new(Line::from(Span::styled(
        format!(" {text}"),
        Style::default()
            .fg(theme.dim)
            .add_modifier(Modifier::ITALIC),
    )));
    frame.render_widget(para, area);
}

// ─── bottom status bar (D7) ────────────────────────────────────────────────

fn render_status_bar(frame: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    let pct = if app.status.input_tokens == 0 {
        0
    } else {
        ((app.status.input_tokens.min(CONTEXT_TOKEN_BUDGET) * 100) / CONTEXT_TOKEN_BUDGET) as u32
    };
    let mut parts: Vec<String> = vec![
        app.status.model.clone(),
        format!("{pct}% context"),
        format!("session {}", short_session(&app.session_id)),
    ];
    if let Some(branch) = &app.git_branch {
        parts.push(branch.clone());
    }

    let mut spans: Vec<Span<'static>> = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(
                "  ·  ".to_string(),
                Style::default().fg(theme.subtle),
            ));
        }
        spans.push(Span::styled(part.clone(), Style::default().fg(theme.dim)));
    }

    if let Some(hint) = &app.status_hint {
        spans.push(Span::styled(
            format!("    {hint}"),
            Style::default()
                .fg(theme.warning)
                .add_modifier(Modifier::ITALIC),
        ));
    }

    let para = Paragraph::new(Line::from(spans));
    frame.render_widget(para, area);
}

// ─── command palette popup ─────────────────────────────────────────────────

fn render_command_palette(frame: &mut Frame, app: &App, input_area: Rect, theme: &Theme) {
    let n = app.palette_matches.len().min(8) as u16;
    if n == 0 {
        return;
    }
    let popup_height = n + 2; // +2 for the border.
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
                .bg(theme.claude_orange)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.text)
        };
        rows.push(Line::from(Span::styled(format!(" /{name} "), style)));
    }

    let para = Paragraph::new(rows).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Commands ")
            .style(Style::default().fg(theme.claude_orange)),
    );
    frame.render_widget(para, area);
}

// ─── permission modal ──────────────────────────────────────────────────────

fn render_permission_modal(frame: &mut Frame, perm: &PendingPermission, area: Rect, theme: &Theme) {
    let modal = centered_rect(60, 30, area);
    frame.render_widget(Clear, modal);

    let body = vec![
        Line::from(Span::styled(
            format!("Tool: {}", perm.tool_name),
            Style::default()
                .fg(theme.permission_blue)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(perm.summary.as_str().to_string()),
        Line::from(""),
        Line::from(Span::styled(
            "[y] Allow once    [a] Always allow    [n / Esc] Reject",
            Style::default().fg(theme.text),
        )),
    ];

    let para = Paragraph::new(body)
        .alignment(Alignment::Left)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Permission required ")
                .style(Style::default().fg(theme.permission_blue)),
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

    /// Render `app` into an 80×24 `TestBackend` and return the rendered
    /// buffer as a UTF-8 grid (one row per line). Used by every render-tier
    /// acceptance test in this module.
    fn render_to_string(app: &App, w: u16, h: u16) -> String {
        let backend = TestBackend::new(w, h);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| render(f, app)).unwrap();
        buffer_to_string(term.backend().buffer())
    }

    fn rows(s: &str) -> Vec<&str> {
        s.split_inclusive('\n').collect()
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

    /// AC-V9 — the bordered "Claude -- session …" title is gone, the help
    /// footer is at row height-2, and the model name is on the bottom row.
    #[test]
    fn idle_layout_has_no_title_and_pinned_footer_and_status() {
        let app = App::new("abcdef1234".into(), "claude-sonnet-4-6".into());
        let s = render_to_string(&app, 80, 24);
        assert!(
            !s.contains("Claude -- session"),
            "old bordered title must be gone:\n{s}"
        );
        let r = rows(&s);
        // Help footer at height-2, model at height-1.
        assert!(
            r[22].contains('?') || r[22].contains('/'),
            "expected hint chars at row 22: {:?}",
            r[22]
        );
        assert!(
            r[23].contains("claude-sonnet-4-6"),
            "expected model at row 23: {:?}",
            r[23]
        );
    }

    /// AC-V10 — empty session shows the welcome banner with version, cwd,
    /// and at least one tip. Pushing a user message hides the banner.
    #[test]
    fn empty_session_shows_welcome_banner_then_disappears() {
        let mut app = App::new("s".into(), "m".into());
        app.set_version("0.1.0");
        app.set_cwd("~/dev/cc-rust");
        let s = render_to_string(&app, 80, 24);
        assert!(s.contains("Welcome to Claude Code"), "{s}");
        assert!(s.contains("v0.1.0"), "{s}");
        assert!(s.contains("cwd: ~/dev/cc-rust"), "{s}");
        let mentions_a_tip = welcome::TIPS.iter().any(|t| s.contains(t));
        assert!(mentions_a_tip, "expected at least one tip; got:\n{s}");

        app.push_user("hi".into());
        let s2 = render_to_string(&app, 80, 24);
        assert!(
            !s2.contains("Welcome to Claude Code"),
            "welcome must hide once a turn starts:\n{s2}"
        );
    }

    /// AC-V11 — each `AppMode` produces the matching footer hint string at
    /// row height-2.
    #[test]
    fn footer_hint_matches_mode() {
        let mut app = App::new("s".into(), "m".into());

        // Input
        let s = render_to_string(&app, 80, 24);
        assert!(rows(&s)[22].contains("? for shortcuts"));

        // Streaming
        app.start_stream();
        let s = render_to_string(&app, 80, 24);
        assert!(rows(&s)[22].contains("esc to interrupt"));

        // PermissionPrompt
        app.mode = AppMode::PermissionPrompt;
        app.permission = Some(PendingPermission {
            tool_name: "Write".into(),
            summary: "/tmp/foo".into(),
        });
        let s = render_to_string(&app, 80, 24);
        assert!(rows(&s)[22].contains("y allow"));

        // CommandPalette
        app.permission = None;
        app.mode = AppMode::CommandPalette;
        let s = render_to_string(&app, 80, 24);
        assert!(rows(&s)[22].contains("↑↓ select"));
    }

    /// AC-V11 bonus — typing into the input shows the claude-orange `>`
    /// gutter prefix.
    #[test]
    fn input_box_shows_orange_gutter() {
        let mut app = App::new("s".into(), "m".into());
        app.input.push_str("hello");
        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| render(f, &app)).unwrap();
        let buf = term.backend().buffer();
        // Input row is layout slot 2 (transcript Min(0) + spinner 1 + input top
        // border). At height=24, slot 2 starts at y = 18 + 1 = 19; the inner
        // text row is y = 20.
        let theme = theme::current();
        let cell = buf[(1u16, 20u16)].clone();
        assert_eq!(cell.symbol(), ">", "missing > gutter glyph at (1,20)");
        assert_eq!(cell.fg, theme.claude_orange);
    }

    /// AC-V12-style anchor — a 4-turn fixture renders the gutter conventions
    /// (no `Claude:` header, `>` on user, `⏺` on tool, `✓`/`✗` ticks, `──
    /// compacted ──` divider).
    #[test]
    fn gutter_conventions_render_for_four_turn_fixture() {
        let mut app = App::new("s".into(), "claude-sonnet-4-6".into());
        app.push_user("first".into());
        app.transcript
            .push(crate::app::TranscriptItem::AssistantText("answer".into()));
        app.push_tool_call("Bash".into(), "ls".into());
        app.push_tool_result("Bash".into(), "file1\nfile2".into(), false);
        app.push_compact_boundary();
        app.push_tool_call("Edit".into(), "foo.rs".into());
        app.push_tool_result("Edit".into(), "patch failed".into(), true);

        let s = render_to_string(&app, 80, 30);
        assert!(
            !s.contains("Claude:"),
            "assistant header must be gone:\n{s}"
        );
        assert!(s.contains("> first"), "user gutter missing:\n{s}");
        assert!(
            s.contains(TOOL_BULLET),
            "tool bullet ({TOOL_BULLET}) missing:\n{s}"
        );
        assert!(s.contains("✓"), "success tick missing:\n{s}");
        assert!(s.contains("✗"), "error tick missing:\n{s}");
        assert!(s.contains("compacted"), "compact divider missing:\n{s}");
    }

    /// AC-V13 — within the same redraw that started a stream, the spinner
    /// row carries a verb. After `finish_stream` the row is blank again.
    #[test]
    fn spinner_row_appears_during_stream_and_clears_after() {
        let mut app = App::new("s".into(), "m".into());
        app.start_stream();
        let s = render_to_string(&app, 80, 24);
        // Spinner row is layout slot 1 (after Min(0) transcript). At h=24 it
        // sits at y = 24 - 6 = 18 (1 spinner + 3 input + 1 footer + 1 status).
        let r = rows(&s);
        let spinner_line = r[18];
        let mentions_a_verb = ["Thinking", "Pondering", "Cogitating", "Working"]
            .iter()
            .any(|v| spinner_line.contains(v));
        assert!(
            mentions_a_verb,
            "spinner row must show a verb during stream: {spinner_line:?}"
        );

        app.finish_stream();
        let s = render_to_string(&app, 80, 24);
        let r = rows(&s);
        // Spinner row blank: should not contain any verb.
        let any_verb = ["Thinking", "Pondering", "Cogitating", "Working"]
            .iter()
            .any(|v| r[18].contains(v));
        assert!(
            !any_verb,
            "spinner row must be blank after stream: {:?}",
            r[18]
        );
    }

    /// BUG-1 regression — the context-% denominator must match the real
    /// window size (200 K tokens, same as `cc_core::DEFAULT_CONTEXT_WINDOW`
    /// and the TS original's `MODEL_CONTEXT_WINDOW_DEFAULT`). A previous
    /// version used 180K, which is the auto-compact threshold — that
    /// inflated the percentage by ~10% and let the status bar show 100%
    /// while the engine still had 20K of room.
    #[test]
    fn context_budget_matches_core_default_context_window() {
        assert_eq!(
            CONTEXT_TOKEN_BUDGET,
            cc_core::model::models::DEFAULT_CONTEXT_WINDOW as u64
        );
        // Extra sanity: at 180K input the status bar must show < 100%.
        let mut app = App::new("sid".into(), "claude-sonnet-4-6".into());
        app.status.input_tokens = 180_000;
        let s = render_to_string(&app, 100, 24);
        let last = rows(&s)[23];
        assert!(
            last.contains("90% context"),
            "expected 90% at 180K/200K, got: {last:?}"
        );
    }

    /// Compact boundary regression — renders as the TS-style
    /// `✻ Conversation compacted (ctrl+o for history)` left-aligned, not
    /// the older centred `── compacted ──` rule.
    #[test]
    fn compact_boundary_matches_original_format() {
        let mut app = App::new("s".into(), "m".into());
        app.push_compact_boundary();
        let s = render_to_string(&app, 80, 24);
        assert!(
            s.contains("✻ Conversation compacted"),
            "missing ✻ + label:\n{s}"
        );
        assert!(
            s.contains("(ctrl+o for history)"),
            "missing shortcut hint:\n{s}"
        );
    }

    /// AC-V14 — the bottom-row status bar surfaces the model name plus
    /// session and (when set) git branch separated by ` · `.
    #[test]
    fn status_bar_shows_model_session_and_branch() {
        let mut app = App::new("e11cf4bf1234".into(), "claude-sonnet-4-6".into());
        app.set_git_branch(Some("phase3/implementation".into()));
        let s = render_to_string(&app, 100, 24);
        let r = rows(&s);
        let last = r[23];
        assert!(last.contains("claude-sonnet-4-6"), "{last}");
        assert!(last.contains("session e11cf4bf"), "{last}");
        assert!(last.contains("phase3/implementation"), "{last}");
    }

    // ─── Pre-existing render tests, updated for the new layout ─────────────

    #[test]
    fn renders_empty_app_without_panic() {
        let app = App::new("abcdef1234".into(), "test-model".into());
        let s = render_to_string(&app, 80, 24);
        // Session id is no longer in a top title, but the bottom status bar
        // still shows the truncated form.
        assert!(s.contains("abcdef12"), "session id missing:\n{s}");
    }

    #[test]
    fn renders_streaming_text_and_cursor() {
        let mut app = App::new("s".into(), "m".into());
        app.start_stream();
        app.on_token("hello world");
        let s = render_to_string(&app, 80, 24);
        assert!(s.contains("hello world"));
    }

    #[test]
    fn multiline_system_message_renders_on_separate_rows() {
        let mut app = App::new("s".into(), "m".into());
        app.push_system("Slash commands:\n  /help   show this help\n  /exit   quit".into());
        let s = render_to_string(&app, 80, 24);
        assert!(s.contains("Slash commands:"));
        assert!(s.contains("/help"));
        assert!(s.contains("/exit"));
    }

    #[test]
    fn long_transcript_pins_latest_content_to_bottom() {
        let mut app = App::new("s".into(), "m".into());
        for i in 0..40 {
            app.push_user(format!("user message number {i}"));
        }
        app.scroll = 0;
        let s = render_to_string(&app, 80, 14);
        assert!(
            s.contains("user message number 39"),
            "latest message should be visible at the bottom:\n{s}"
        );
    }

    #[test]
    fn renders_permission_modal_when_set() {
        let mut app = App::new("s".into(), "m".into());
        app.permission = Some(PendingPermission {
            tool_name: "Write".into(),
            summary: "/tmp/foo.txt".into(),
        });
        let s = render_to_string(&app, 80, 24);
        assert!(s.contains("Permission required"));
        assert!(s.contains("Write"));
    }

    #[test]
    fn renders_tool_call_and_result() {
        let mut app = App::new("s".into(), "m".into());
        app.push_tool_call("Bash".into(), "ls -la".into());
        app.push_tool_result("Bash".into(), "file1.rs\nfile2.rs".into(), false);
        let s = render_to_string(&app, 80, 24);
        assert!(s.contains("Bash"));
        assert!(s.contains("ls -la"));
    }
}
