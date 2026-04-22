//! Terminal lifecycle and inline-viewport plumbing for the TUI.
//!
//! Owns the bits that sit between `run_tui`'s main loop and the raw terminal:
//!   * panic hook + file-log installers (so crashes under raw mode still
//!     produce a readable post-mortem),
//!   * inline / fullscreen viewport state tracking,
//!   * `draw_with_resize` — the single draw helper that decides when to
//!     resize or push content into terminal scrollback,
//!   * `flush_to_scrollback` + `stable_streaming_prefix_end` — the stable-
//!     prefix flushing logic that keeps live content short during streams.
//!
//! Extracted from `lib.rs` (2026-04-20 refactor). No behavior change.

use crate::app::App;
use ratatui::layout::Rect;
use ratatui::Terminal;

/// Fixed inline viewport height. We no longer grow it with content:
///
///   - Streaming paragraphs are flushed to terminal scrollback the moment
///     they cross a stable `\n\n` boundary (`flush_to_scrollback`), so
///     `streaming_text` stays short and the viewport never needs to bulge
///     to hold the whole response.
///
///   - Growing + later shrinking the inline viewport produced visual
///     artefacts (user-reported empty-block below completed content,
///     2026-04-20) because Ratatui's `Terminal::resize` interacts oddly
///     with Inline viewport origin tracking after `insert_before` has
///     shifted the viewport around.
///
/// Keeping the viewport at a small, stable size sidesteps both problems
/// and matches what Claude Code's Ink TUI does: the live area is just
/// input + chrome, content lives in scrollback.
pub(crate) const FIXED_INLINE_ROWS: u16 = 8;

/// Active viewport flavour. `Fullscreen` is the fallback we land in when
/// the inline init's DSR-cursor probe times out (slow / nested terminals).
/// In Fullscreen mode, `flush_to_scrollback` and the lazy-resize logic are
/// no-ops because Ratatui's `Viewport::Fullscreen` doesn't support
/// `Terminal::insert_before` (it'd be a silent no-op anyway) and the area
/// is fixed to the terminal size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ViewportKind {
    Inline,
    Fullscreen,
}

/// Snapshot of the most recent inline viewport size + the terminal size it
/// was sized for. Held across draws so we resize only on real changes.
pub(crate) struct ViewportState {
    pub(crate) height: u16,
    pub(crate) term_size: (u16, u16),
    pub(crate) kind: ViewportKind,
}

/// Cap a desired inline viewport height to `terminal_height - 1` so a row of
/// breathing room remains between the prior shell prompt and our top edge.
/// Floors at 4 so we never collapse below "input + footer" usability.
pub(crate) fn clamp_viewport(desired: u16, term_height: u16) -> u16 {
    let max = term_height.saturating_sub(1).max(4);
    desired.clamp(4, max)
}

/// Append a one-line note to `~/.claude/cc-tui-crash.log` recording that
/// the inline-viewport init had to fall back to Fullscreen. We piggy-back
/// on the crash log rather than spawning a third file because users
/// already know to check that file when something looks wrong, and the
/// fallback is the kind of thing they'd want to see alongside crashes.
pub(crate) fn log_inline_fallback(reason: &str) -> std::io::Result<()> {
    if let Some(mut path) = dirs::home_dir() {
        path.push(".claude");
        std::fs::create_dir_all(&path)?;
        path.push("cc-tui-crash.log");
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            use std::io::Write;
            let now = chrono::Utc::now().to_rfc3339();
            let _ = writeln!(
                f,
                "[{now}] inline-viewport init fell back to Fullscreen: {reason}"
            );
        }
    }
    Ok(())
}

/// Route `tracing` events to `$CC_TUI_LOG_FILE` when that env var is set.
/// No-op when the env var is missing (avoids spamming a stray file during
/// normal use) and no-op after the first successful install.
///
/// This is the canonical way to get structured logs out of a raw-mode TUI:
/// stderr is unusable because it would corrupt the screen, and a file sink
/// is easy to `tail -f` from another terminal or assert-on from PTY tests.
pub(crate) fn install_file_log() {
    use std::sync::OnceLock;
    static INSTALLED: OnceLock<()> = OnceLock::new();
    let path = match std::env::var("CC_TUI_LOG_FILE") {
        Ok(v) if !v.is_empty() => v,
        _ => return,
    };
    INSTALLED.get_or_init(|| {
        // `tracing-subscriber` is already a workspace dep; we only need the
        // file layer here. Use `RUST_LOG` if the user set it, otherwise
        // default to info for cc-* crates + warn for everything else to keep
        // the log signal/noise sane.
        if let Ok(f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let filter = std::env::var("RUST_LOG")
                .unwrap_or_else(|_| "cc_tui=debug,cc_query=info,warn".to_string());
            // Build a minimal subscriber; ignore errors if something else
            // already set a global one.
            let _ = tracing_subscriber::fmt()
                .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
                .with_writer(std::sync::Mutex::new(f))
                .with_ansi(false)
                .with_target(true)
                .try_init();
        }
    });
}

/// Install a global panic hook that restores the terminal before letting the
/// default hook fire, and also writes the panic info + backtrace to a crash
/// log under `$HOME/.claude/cc-tui-crash.log`. Idempotent.
///
/// Why: without this, a panic in any render/update path scrambles the
/// terminal (raw mode still on) and the panic message gets over-written
/// before the user can read it. The crash log is the *only* reliable way
/// to get a post-mortem on a real-world crash. After a crash, the user
/// should `cat ~/.claude/cc-tui-crash.log` to see the stack.
pub(crate) fn install_panic_hook() {
    use std::sync::OnceLock;
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // 1. Drop raw mode so stderr is readable and the cursor returns.
            let _ = crossterm::terminal::disable_raw_mode();
            // Emit a newline so the default hook's stderr output doesn't
            // land in the middle of the last rendered row.
            eprintln!();

            // 2. Capture a backtrace. `std::backtrace::Backtrace::force_capture`
            //    always captures, irrespective of RUST_BACKTRACE. We want
            //    full info regardless of the user's env.
            let backtrace = std::backtrace::Backtrace::force_capture();

            // 3. Write the full report to ~/.claude/cc-tui-crash.log.
            //    Append so multiple crashes in a session are preserved.
            if let Some(mut path) = dirs::home_dir() {
                path.push(".claude");
                let _ = std::fs::create_dir_all(&path);
                path.push("cc-tui-crash.log");
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                {
                    use std::io::Write;
                    let now = chrono::Utc::now().to_rfc3339();
                    let _ = writeln!(f, "\n=== cc-tui panic at {now} ===");
                    let _ = writeln!(f, "{info}");
                    let _ = writeln!(f, "--- backtrace ---\n{backtrace}");
                    // Surface where the log lives so the user doesn't have
                    // to hunt for it.
                    eprintln!("cc-tui crashed — details in {}", path.display());
                }
            }

            // 4. Chain to the original hook so stderr still shows the panic
            //    in case the user can read it (e.g. ran under `tee`).
            previous(info);
        }));
    });
}

/// Draw the frame, resizing the inline viewport **only** when necessary:
///
///   1. The terminal itself resized (SIGWINCH) — always resize to the new
///      width and recompute the clamp against the new height.
///   2. The content estimate grew beyond the current viewport *and* we
///      haven't hit the terminal-height cap.
///
/// Specifically we do **not** shrink the viewport as the user types or the
/// model streams — that produced a nasty flicker where the inline area
/// pulsed up/down by one row on every delta, pushing the prior frame up
/// into scrollback each time. Ratatui's `Paragraph.scroll` inside
/// `render_transcript` already handles "too much content for the viewport"
/// by pinning the tail to the bottom, so shrinking is never required to
/// stay correct — only growing is.
pub(crate) fn draw_with_resize<B>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    vp: &mut ViewportState,
) -> cc_core::CcResult<()>
where
    B: ratatui::backend::Backend,
{
    // In Fullscreen-fallback mode, the viewport is fixed to the terminal
    // size and `insert_before` is a Ratatui no-op. Skip the flush + resize
    // dance entirely; just draw. `render_transcript` already handles the
    // "render everything" case correctly because `app.emitted_to_scrollback`
    // stays at 0 (we never advance it in Fullscreen mode).
    if vp.kind == ViewportKind::Fullscreen {
        tracing::debug!(
            mode = ?app.mode,
            streaming_len = app.streaming_text.len(),
            transcript_items = app.transcript.len(),
            "draw (Fullscreen)"
        );
        terminal
            .draw(|frame| crate::render::render(frame, app))
            .map_err(|e| cc_core::CcError::Other(format!("terminal draw error: {e}")))?;
        return Ok(());
    }

    let term_size = crossterm::terminal::size()
        .map_err(|e| cc_core::CcError::Other(format!("terminal size: {e}")))?;

    // Step 1: flush finalized transcript items to terminal scrollback before
    // we draw. After this returns, the viewport has *no* transcript items
    // to render — just live state (streaming text, spinner, input, chrome).
    //
    // EXCEPT while a permission dialog is up. `insert_before` issues
    // scroll-region sequences to make room above the viewport; those
    // interact badly with the centered modal overlay (the modal paints
    // a `Clear` rect, then `insert_before` scrolls region 0..viewport_top
    // which shifts already-painted modal chrome into scrollback). User
    // screenshots (2026-04-21) showed the permission help footer,
    // spinner row, and input-box borders ending up in scrollback with
    // the overlapping "y allow · a always · n deny" + streaming text
    // artefact. Deferring the flush is safe: items stay in
    // `transcript[emitted..]` and get rendered in-frame by
    // `render_transcript` until the user answers y/a/n and `app.mode`
    // leaves `PermissionPrompt`, at which point the next draw flushes
    // everything to scrollback cleanly.
    if app.mode != crate::app::AppMode::PermissionPrompt {
        flush_to_scrollback(terminal, app, term_size.0)?;
    }

    // Viewport size is fixed (see FIXED_INLINE_ROWS). We only react to real
    // SIGWINCH events (terminal width or height actually changed) — the
    // height we pass to the terminal is still clamped to `term_height - 1`
    // in case the user shrinks their terminal below our fixed size.
    let terminal_resized = term_size != vp.term_size;
    if terminal_resized {
        let new_height = clamp_viewport(vp.height, term_size.1);
        let _ = terminal.resize(Rect::new(0, 0, term_size.0, new_height));
        vp.height = new_height;
        vp.term_size = term_size;
    }

    tracing::debug!(
        mode = ?app.mode,
        streaming_len = app.streaming_text.len(),
        transcript_items = app.transcript.len(),
        viewport_h = vp.height,
        "draw (Inline)"
    );
    terminal
        .draw(|frame| crate::render::render(frame, app))
        .map_err(|e| cc_core::CcError::Other(format!("terminal draw error: {e}")))?;
    Ok(())
}

/// Push any transcript items newer than `app.emitted_to_scrollback` into
/// the terminal scrollback via `Terminal::insert_before`.
///
/// This is what makes the TUI behave like a normal inline CLI: completed
/// messages flow into the user's terminal scrollback (so they can scroll
/// up with the terminal's own mouse wheel / Shift+PageUp / search) while
/// the inline viewport only holds the currently-active state. Before this
/// change, anything past `viewport_rows - chrome` fell off the top of
/// `Paragraph.scroll` and was gone forever.
///
/// Silently drops insert_before errors — in inline mode they mean the
/// terminal is too small to accept the prepend. The item stays in
/// `transcript[emitted..]` and we'll retry next draw, which is the right
/// behaviour: content isn't lost, just deferred until there's room.
fn flush_to_scrollback<B>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    width: u16,
) -> cc_core::CcResult<()>
where
    B: ratatui::backend::Backend,
{
    // NOTE: do NOT early-return when all transcript items are already
    // flushed. During streaming, `transcript` may be empty (no finalised
    // assistant item yet) but `streaming_text` has content that still
    // needs stable-prefix flushing. The early-return bug caused the 2026-
    // 04-20 "long stream scrolls off viewport" report: nothing ever
    // flushed until `turn_complete` fired.
    let theme = crate::theme::current();
    let start = app.emitted_to_scrollback;
    for idx in start..app.transcript.len() {
        let item = &app.transcript[idx];
        let lines = crate::render::render_item_lines(item, width, &theme);
        let row_count = lines.len() as u16;
        if row_count == 0 {
            app.emitted_to_scrollback = idx + 1;
            continue;
        }
        let insert_result = terminal.insert_before(row_count, |buf| {
            let paragraph = ratatui::widgets::Paragraph::new(lines.clone())
                .wrap(ratatui::widgets::Wrap { trim: false });
            ratatui::widgets::Widget::render(paragraph, buf.area, buf);
        });
        if insert_result.is_err() {
            // Terminal too small right now; try again next draw. Leave the
            // index un-advanced so we don't skip this item.
            tracing::debug!("insert_before failed at item {idx}; deferring to next draw");
            return Ok(());
        }
        app.emitted_to_scrollback = idx + 1;
    }

    // Also flush "stable" portion of the in-flight streaming text.
    //
    // Without this, when an assistant response is longer than the viewport
    // height, earlier paragraphs scroll off the top of `Paragraph.scroll`'s
    // pinned-to-bottom view and are unrecoverable — they never made it into
    // terminal scrollback because no `insert_before` was ever called for
    // them (only finalized transcript items went through flush).
    //
    // User-visible symptom (2026-04-20 screenshots): during a long stream
    // the viewport showed paragraphs N, N+1, N+2 at 10 s, then N+3, N+4,
    // N+5 at 20 s (earlier ones gone). At turn_complete the full text
    // flushed at once — leaving visible only the tail and whatever the
    // terminal's own scrollback happened to pick up during the
    // replacement.
    //
    // Fix: find the newest "safe flush point" in `streaming_text` — the
    // last blank-line boundary (`\n\n`) that is NOT inside an open fenced
    // code block. Everything up to that point is guaranteed stable
    // (line-scoped markdown blocks are complete; no open fence spanning
    // the boundary) and can be emitted to scrollback. The unstable tail
    // stays in `streaming_text` for the viewport renderer.
    if !app.streaming_text.is_empty() {
        let stream_len_before = app.streaming_text.len();
        let end_opt = stable_streaming_prefix_end(&app.streaming_text);
        tracing::debug!(
            stream_len_before,
            flush_end = ?end_opt,
            "streaming flush pass"
        );
        if let Some(end) = end_opt {
            if end > 0 {
                let stable = app.streaming_text[..end].to_string();
                let tail = app.streaming_text[end..].to_string();
                let lines = crate::markdown::render_markdown(&stable);
                let row_count = lines.len() as u16;
                tracing::debug!(
                    stable_bytes = stable.len(),
                    row_count,
                    tail_bytes = tail.len(),
                    "streaming flush insert_before"
                );
                if row_count > 0 {
                    let insert_result = terminal.insert_before(row_count, |buf| {
                        let para = ratatui::widgets::Paragraph::new(lines.clone())
                            .wrap(ratatui::widgets::Wrap { trim: false });
                        ratatui::widgets::Widget::render(para, buf.area, buf);
                    });
                    match insert_result {
                        Ok(()) => {
                            app.streaming_text = tail;
                            tracing::debug!("streaming flush succeeded");
                        }
                        Err(e) => {
                            tracing::debug!(error = %e, "streaming flush failed");
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

/// Scan `text` and return the byte offset of the newest "safe to flush"
/// boundary, or `None` if no such boundary exists yet.
///
/// A boundary is a blank line (`\n\n`) that is NOT inside a fenced code
/// block. Content before the boundary cannot change with future appends
/// (all line-scoped blocks are complete), so it's safe to push to
/// terminal scrollback incrementally while the stream continues.
///
/// If the parser is inside an open fence at the boundary candidate, we
/// skip it — the code block will finalise later and we'd rather render
/// it complete than split across an `insert_before` call (which would
/// leave an orphan "…streaming" marker frozen in scrollback).
fn stable_streaming_prefix_end(text: &str) -> Option<usize> {
    let mut in_fence = false;
    let mut last_safe_end: Option<usize> = None;
    let mut pos = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
        }
        // A blank line (just `\n` or CRLF) outside a fence is the stable
        // marker. Everything up to and including this blank line will not
        // be rewritten by future tokens.
        if !in_fence && line.trim().is_empty() && line.ends_with('\n') {
            last_safe_end = Some(pos + line.len());
        }
        pos += line.len();
    }
    last_safe_end
}

#[cfg(test)]
mod streaming_flush_tests {
    //! Anchor the "flush stable prefix to scrollback during streaming"
    //! invariant. User reported via screenshots (2026-04-20) that during
    //! a long assistant stream the earlier paragraphs vanished from view
    //! — they were scrolled off `Paragraph.scroll`'s pinned-to-bottom
    //! window without ever being handed to `terminal.insert_before`.
    //! These tests guarantee `stable_streaming_prefix_end` identifies
    //! flushable boundaries.
    use super::*;

    #[test]
    fn no_blank_line_yet_no_flush() {
        assert_eq!(stable_streaming_prefix_end("hello world"), None);
        assert_eq!(stable_streaming_prefix_end("one\ntwo\nthree"), None);
    }

    #[test]
    fn single_blank_line_marks_flush_point() {
        let s = "para one\n\npara two in progress";
        let end = stable_streaming_prefix_end(s).expect("expected flush point");
        // Everything up to and including the blank line is stable.
        assert_eq!(&s[..end], "para one\n\n");
        assert_eq!(&s[end..], "para two in progress");
    }

    #[test]
    fn latest_blank_line_wins_across_multiple_paragraphs() {
        let s = "p1\n\np2\n\np3 still streaming";
        let end = stable_streaming_prefix_end(s).unwrap();
        assert_eq!(&s[..end], "p1\n\np2\n\n");
    }

    #[test]
    fn blank_line_inside_open_fence_is_not_a_flush_point() {
        // Blank line between `code line 1` and `code line 2` is *inside*
        // an open fence — flushing here would split the block and render
        // a broken "…streaming" marker.
        let s = "para\n\n```rust\nline1\n\nline2\n";
        let end = stable_streaming_prefix_end(s).unwrap();
        // Only the blank before the fence qualifies.
        assert_eq!(&s[..end], "para\n\n");
    }

    #[test]
    fn closed_fence_releases_later_blank_line() {
        let s = "para1\n\n```rust\nlet x = 1;\n```\n\nmore";
        let end = stable_streaming_prefix_end(s).unwrap();
        // The blank AFTER the closed fence is the newer safe point.
        assert_eq!(&s[..end], "para1\n\n```rust\nlet x = 1;\n```\n\n");
    }

    #[test]
    fn blank_at_end_only_still_counts() {
        let s = "hello\n\n";
        let end = stable_streaming_prefix_end(s).unwrap();
        assert_eq!(end, s.len());
    }
}

#[cfg(test)]
mod viewport_tests {
    //! Anchor the inline-viewport resize policy: grow on demand, follow
    //! SIGWINCH, but do NOT shrink per stream delta (which caused the
    //! mid-chat flicker).
    use super::*;

    /// Regression: `flush_to_scrollback` must not advance
    /// `app.emitted_to_scrollback` while a permission dialog is up.
    /// Scroll-region sequences from `insert_before` interact badly with
    /// the centered modal overlay and left the permission help footer +
    /// streaming text + input-box chrome visible in scrollback
    /// (2026-04-21 screenshot). Deferring the flush preserves all
    /// pending items — they stay in `transcript[emitted..]` and render
    /// in-frame until the user answers y/a/n.
    #[test]
    fn flush_is_deferred_under_permission_prompt() {
        use crate::app::{App, AppMode, TranscriptItem};
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = App::new("s".into(), "m".into());
        app.transcript.push(TranscriptItem::UserMessage("hi".into()));
        // Simulate an in-flight permission prompt — mode is what the
        // real handler flips to when PermissionRequest arrives.
        app.mode = AppMode::PermissionPrompt;

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut vp = ViewportState {
            height: 8,
            term_size: (80, 24),
            kind: ViewportKind::Inline,
        };

        draw_with_resize(&mut terminal, &mut app, &mut vp).unwrap();

        assert_eq!(
            app.emitted_to_scrollback, 0,
            "flush_to_scrollback must not advance emitted_to_scrollback \
             while a permission dialog is up; got {}",
            app.emitted_to_scrollback
        );
        assert_eq!(
            app.transcript.len(),
            1,
            "transcript item must still be present for in-frame rendering"
        );
    }

    #[test]
    fn clamp_viewport_never_produces_invalid_range() {
        // Terminal smaller than our minimum: must floor at 4 without panic.
        assert_eq!(clamp_viewport(20, 3), 4);
        assert_eq!(clamp_viewport(20, 0), 4);
        // Terminal larger than our desired: pass through.
        assert_eq!(clamp_viewport(10, 30), 10);
        // Desired larger than terminal cap: clamp to terminal - 1.
        assert_eq!(clamp_viewport(100, 30), 29);
        // Both at boundary: just below terminal height.
        assert_eq!(clamp_viewport(29, 30), 29);
    }
}
