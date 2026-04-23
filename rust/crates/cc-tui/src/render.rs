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

    // Welcome banner: rendered in-frame ONLY when `run_tui` stashed one
    // on `App` — that happens exclusively on the Fullscreen-fallback
    // path, because Inline mode pushes the banner into native terminal
    // scrollback via `insert_before` and never populates this field.
    // Once the user submits a turn, `is_empty_session()` flips false and
    // the banner naturally hides without further bookkeeping.
    if app.is_empty_session() {
        if let Some(banner) = &app.welcome_banner {
            lines.extend(banner.iter().cloned());
        }
    }
    let _ = theme; // theme still threaded for sub-renderers below

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

    // Chat UX: latest content sits just above the input box. When the
    // transcript is *taller* than the viewport, Paragraph.scroll pins the
    // tail to the bottom already (via `max_scroll.saturating_sub(app.scroll)`).
    // When it's *shorter* (common in Fullscreen-fallback mode at session
    // start — content ~10 rows, viewport ~35 rows), we need to pre-pad with
    // blank rows so the content bottom-aligns instead of pinning to the
    // top and leaving a big gap above the input box. Only the fallback
    // path actually hits this: Inline viewport shrinks to content so there
    // is no extra slack.
    if total_rows < viewport_rows {
        let pad = viewport_rows - total_rows;
        let mut padded: Vec<Line<'static>> = Vec::with_capacity(pad + lines.len());
        for _ in 0..pad {
            padded.push(Line::from(""));
        }
        padded.extend(lines);
        let para = Paragraph::new(padded).wrap(Wrap { trim: false });
        frame.render_widget(para, area);
        return;
    }

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

    let gutter_cells: u16 = 2; // "> "
                               // Content cells available for the buffer: total - 2 borders - gutter.
    let inner_width = area.width.saturating_sub(2).saturating_sub(gutter_cells);

    // Reflow the viewport offset so the caret stays inside the window
    // before we slice. Uses interior mutability so we can call this from
    // `&App` render code.
    app.reflow_input_viewport(inner_width);
    let offset = app.input_view_offset.get();

    // Inner content: gutter glyph + (sliced buffer or placeholder).
    //
    // The gutter reflects the current input "mode prefix": `!` switches
    // us into bash mode (see Submit arm), so the gutter becomes `$ ` in
    // cyan to mirror a shell prompt. Otherwise we show the claude-orange
    // `> ` we use for regular user input.
    let bash_mode = app.input.starts_with('!');
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(2);
    let (gutter, gutter_color) = if bash_mode {
        // Same pink as the Bash tool card so the user sees at a glance
        // this input will turn into a shell command (AC-V12 palette).
        ("$ ", theme.bash_pink)
    } else {
        ("> ", theme.claude_orange)
    };
    spans.push(Span::styled(
        gutter.to_string(),
        Style::default()
            .fg(gutter_color)
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
        // Slice the buffer by display cells starting at `offset`. Walk
        // chars skipping until we've passed `offset` cells, then emit up
        // to `inner_width` cells of content. This keeps long buffers
        // from truncating silently past the right edge.
        let mut shown = String::new();
        let mut cum = 0usize;
        let mut emitted = 0usize;
        let inner = inner_width as usize;
        for ch in app.input.chars() {
            let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if cum < offset {
                cum += w;
                continue;
            }
            if emitted + w > inner {
                break;
            }
            shown.push(ch);
            emitted += w;
            cum += w;
        }
        spans.push(Span::styled(shown, Style::default().fg(theme.text)));
    }

    let para = Paragraph::new(Line::from(spans)).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color)),
    );
    frame.render_widget(para, area);

    // ── Caret ────────────────────────────────────────────────────────────
    //
    // Hide the caret while a permission prompt is up — keystrokes there
    // map to the y/a/n shortcut, not free text — but show it everywhere
    // else (Input, CommandPalette, even Streaming where the user can
    // queue follow-up input).
    //
    // Caret display col = `caret_col - offset` (relative to the visible
    // window). The outer clamp stays as a safety net but should not
    // normally fire because reflow keeps the caret inside the window.
    if !matches!(app.mode, AppMode::PermissionPrompt) {
        let caret_col = app.input_caret_display_col();
        let rel = caret_col.saturating_sub(offset) as u16;
        let cursor_x = area
            .x
            .saturating_add(1) // step past left border
            .saturating_add(gutter_cells)
            .saturating_add(rel)
            .min(area.x.saturating_add(area.width).saturating_sub(2));
        let cursor_y = area.y.saturating_add(1); // step past top border
        frame.set_cursor_position((cursor_x, cursor_y));
    }
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
    let n_matches = app.palette_matches.len().min(8);
    if n_matches == 0 {
        return;
    }

    // Clamp the popup so it stays inside the frame buffer. In Inline
    // mode the viewport is FIXED_INLINE_ROWS tall (8), so the rows
    // available ABOVE the input box can be as few as 2. Without this
    // clamp `frame.render_widget(Clear, area)` writes to absolute
    // coordinates that fall outside `frame.area()` and ratatui's
    // `Buffer::index_of` panics with "index outside of buffer".
    // Repro: 8 matches → popup_height=10, input_area.y=94, frame.y=91
    // → y=84 < 91 → panic.
    let frame_top = frame.area().y;
    let rows_above_input = input_area.y.saturating_sub(frame_top);
    // Need at least 3 rows: top border + 1 entry + bottom border.
    if rows_above_input < 3 {
        return;
    }
    let max_entries_by_room = (rows_above_input - 2) as usize;
    let n = n_matches.min(max_entries_by_room) as u16;
    if n == 0 {
        return;
    }
    let popup_height = n + 2; // +2 for the border.
    let width = input_area.width.clamp(20, 50);
    let x = input_area.x;
    // popup_height ≤ rows_above_input by construction above, so this
    // subtraction is exact (no saturating). Keeping saturating_sub
    // anyway as belt-and-braces against future edits to the clamp.
    let y = input_area.y.saturating_sub(popup_height);
    let area = Rect {
        x,
        y,
        width,
        height: popup_height,
    };
    frame.render_widget(Clear, area);

    // Entries read differently depending on source: command palette
    // shows `/name`, file palette shows `@path` (trailing `/` on dirs
    // is already in the match list from `list_cwd_files`).
    let prefix = match app.palette_kind {
        crate::app::PaletteKind::Commands => "/",
        crate::app::PaletteKind::Files => "@",
    };
    let title = match app.palette_kind {
        crate::app::PaletteKind::Commands => " Commands ",
        crate::app::PaletteKind::Files => " Files ",
    };

    let mut rows: Vec<Line<'static>> = Vec::with_capacity(n as usize);
    for (i, name) in app.palette_matches.iter().take(n as usize).enumerate() {
        let style = if i == app.palette_selected {
            Style::default()
                .fg(Color::Black)
                .bg(theme.claude_orange)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.text)
        };
        rows.push(Line::from(Span::styled(format!(" {prefix}{name} "), style)));
    }

    let para = Paragraph::new(rows).block(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
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

    /// AC-V10 (Inline default) — when the caller has NOT stashed a
    /// welcome banner on `App`, the frame contains no banner. The
    /// Inline-viewport path in `run_tui` pushes the banner into native
    /// terminal scrollback via `insert_before` and leaves
    /// `app.welcome_banner == None`, so the frame itself stays banner-
    /// free. This test pins that empty-state invariant.
    #[test]
    fn welcome_banner_is_not_in_ratatui_frame_under_inline() {
        let mut app = App::new("s".into(), "m".into());
        app.set_version("0.1.0");
        app.set_cwd("~/dev/cc-rust");
        let s = render_to_string(&app, 80, 24);
        assert!(
            !s.contains("Welcome to Claude Code"),
            "welcome leaked into frame when no banner is stashed: {s}"
        );
    }

    /// AC-V10 (Fullscreen fallback) — when the caller has stashed a
    /// banner on `App` (the Fullscreen-fallback path in `run_tui`), the
    /// banner renders inside the transcript area and hides automatically
    /// once a transcript item arrives.
    #[test]
    fn welcome_banner_renders_in_ratatui_frame_under_fullscreen() {
        use ratatui::text::{Line, Span};
        let mut app = App::new("s".into(), "m".into());
        app.set_version("0.1.0");
        app.set_cwd("~/dev/cc-rust");
        app.set_welcome_banner(vec![
            Line::from(Span::raw("Welcome to Claude Code".to_string())),
            Line::from(Span::raw("v0.1.0".to_string())),
            Line::from(Span::raw("cwd: ~/dev/cc-rust".to_string())),
        ]);

        // Empty session → banner is visible.
        let s = render_to_string(&app, 80, 24);
        assert!(
            s.contains("Welcome to Claude Code"),
            "welcome banner missing in Fullscreen empty-session frame:\n{s}"
        );

        // Once a transcript item lands, is_empty_session flips false and
        // the banner hides even though `app.welcome_banner` is still set.
        app.push_user("hi".into());
        let s2 = render_to_string(&app, 80, 24);
        assert!(
            !s2.contains("Welcome to Claude Code"),
            "welcome must hide once a turn starts:\n{s2}"
        );
    }

    /// Caret must sit on the input row at column = "> " gutter +
    /// already-typed text width. Without this the input box looks
    /// frozen and the user can't tell where keystrokes will land.
    /// Reported by user via screenshot 2026-04-21.
    #[test]
    fn input_caret_position_tracks_typed_text() {
        use ratatui::backend::Backend;
        use ratatui::layout::Position;
        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();

        // Empty input → cursor right after the "> " gutter (x=3 inside
        // the bordered box at x=0). Input row sits at y = h - 5 = 19;
        // caret is one row down from the top border, so y = 20.
        let app_empty = App::new("s".into(), "m".into());
        term.draw(|f| render(f, &app_empty)).unwrap();
        assert_eq!(
            term.backend_mut().get_cursor_position().unwrap(),
            Position::new(3, 20),
            "caret not at gutter on empty input"
        );

        // Typed "hello" → cursor advances 5 cells.
        let mut app_typed = App::new("s".into(), "m".into());
        app_typed.set_input("hello");
        term.draw(|f| render(f, &app_typed)).unwrap();
        assert_eq!(
            term.backend_mut().get_cursor_position().unwrap(),
            Position::new(8, 20),
            "caret not after typed text"
        );

        // CJK ideograph (2 cells wide) → cursor advances 2.
        let mut app_cjk = App::new("s".into(), "m".into());
        app_cjk.set_input("中");
        term.draw(|f| render(f, &app_cjk)).unwrap();
        assert_eq!(
            term.backend_mut().get_cursor_position().unwrap(),
            Position::new(5, 20),
            "caret didn't account for CJK width"
        );

        // Left-arrow moves caret one char back — for "hello" with caret
        // at end (x=8), Left should land it at x=7 (between "l" and
        // "o"). Pins the regression for "can't move cursor left/right"
        // reported 2026-04-22.
        let mut app_mid = App::new("s".into(), "m".into());
        app_mid.set_input("hello");
        app_mid.input_cursor_left();
        term.draw(|f| render(f, &app_mid)).unwrap();
        assert_eq!(
            term.backend_mut().get_cursor_position().unwrap(),
            Position::new(7, 20),
            "caret didn't move left after input_cursor_left()"
        );

        // Home jumps caret to start; End jumps to end.
        let mut app_home = App::new("s".into(), "m".into());
        app_home.set_input("hello");
        app_home.input_cursor_home();
        term.draw(|f| render(f, &app_home)).unwrap();
        assert_eq!(
            term.backend_mut().get_cursor_position().unwrap(),
            Position::new(3, 20),
            "Home didn't land caret at gutter"
        );
        app_home.input_cursor_end();
        term.draw(|f| render(f, &app_home)).unwrap();
        assert_eq!(
            term.backend_mut().get_cursor_position().unwrap(),
            Position::new(8, 20),
            "End didn't return caret to buffer end"
        );
    }

    /// Regression: rendering the slash-command palette at the small
    /// FIXED_INLINE_ROWS=8 viewport height with a maximum-sized 8-entry
    /// match list panicked with
    /// `index outside of buffer: the area is Rect{ x:0, y:91, h:8 }
    /// but index is (0, 84)` because the palette popup was placed at
    /// `input_area.y - (n+2)` without bounding to the frame top.
    /// User-supplied stack on 2026-04-21.
    #[test]
    fn palette_popup_does_not_panic_on_8_row_viewport() {
        use crate::app::AppMode;
        let mut app = App::new("s".into(), "m".into());
        // Mimic the post-`/` state: mode = CommandPalette, 8 matches
        // (the maximum the popup ever shows). Real palette_matches
        // contents don't matter for the OOB check — only the count.
        app.mode = AppMode::CommandPalette;
        app.set_input("/");
        app.palette_matches = (0..8).map(|i| format!("cmd{i}")).collect();
        // FIXED_INLINE_ROWS = 8 in the runtime; reproduce that height
        // here. Width matches the user's report (354) so any width-
        // dependent regression also lands.
        let _ = render_to_string(&app, 354, 8);
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

    /// fix-tui-help-wrap-indent: when the `/help` notice is rendered on
    /// an 80-col terminal, no non-blank row of the notice must start at
    /// column 0 — the reader's eye needs a leading indent for the
    /// continuation to read as "still inside the help block". This
    /// pins the behaviour introduced by the `format_entry` two-line
    /// form plus `push_system_notice`'s 3-space continuation prefix.
    #[test]
    fn help_notice_no_zero_indent_continuation() {
        use crate::commands::CommandRegistry;
        let mut app = App::new("s".into(), "m".into());
        let reg = CommandRegistry::empty();
        app.push_system(reg.help_text());
        // 80×30: transcript area is rows 0..25 (chrome = spinner+input+footer+status = 5).
        let s = render_to_string(&app, 80, 30);
        let all = rows(&s);
        let transcript = &all[..all.len().saturating_sub(6)];
        for (i, row) in transcript.iter().enumerate() {
            let body = row.trim_end_matches('\n');
            if body.trim().is_empty() {
                continue;
            }
            // Every populated transcript row in the notice starts with
            // notice prefix or command-entry indent. The failure mode we
            // guard against is the wrap tail landing at col 0.
            assert!(
                body.starts_with(' '),
                "transcript row {i} begins at col 0: {body:?}\nfull frame:\n{s}"
            );
        }
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
