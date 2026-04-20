//! cc-tui — interactive terminal UI for `claude`.
//!
//! Absorbs cc-commands (slash command registry) per Phase 2 Decision 3.
//! Provides Ratatui-based TUI with streaming render, permission dialogs,
//! and slash command dispatch.

pub mod action;
pub mod app;
pub mod commands;
pub mod diff;
pub mod event;
pub mod keybindings;
pub mod markdown;
pub mod prompter;
pub mod render;
pub mod theme;
pub mod welcome;

pub use action::{update, AppAction, UpdateContext, UpdateResult};
pub use app::{App, AppMode, PendingPermission, StatusLine, TranscriptItem};
pub use commands::{CommandContext, CommandOutcome, CommandRegistry};
pub use event::AppEvent;
pub use keybindings::Keybindings;
pub use prompter::ChannelPrompter;

use std::io;
use std::sync::Arc;

use cc_core::{AppEvent as CoreEvent, MessageParam};
use cc_query::QueryEngine;
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::{Terminal, TerminalOptions, Viewport};
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;

/// Configuration for the TUI entry point.
pub struct TuiConfig {
    pub model: String,
    pub session_id: String,
    /// Fully-constructed engine (built by main with all tools, hooks, session).
    pub engine: QueryEngine,
    /// Conversation history (may contain resumed messages).
    pub messages: Vec<MessageParam>,
    /// Root cancellation token (user-level Ctrl+C, not per-turn).
    pub cancel: CancellationToken,
    /// Pre-built command registry (skills discovered from config dir).
    pub commands: CommandRegistry,
    /// Runtime context for command execution.
    pub command_ctx: CommandContext,
    /// Sender half of the engine→TUI event channel.
    /// Must be the same sender used by `ChannelPrompter` so permission
    /// requests and streaming events share a single receiver.
    pub events_tx: mpsc::Sender<CoreEvent>,
    /// Receiver half of the engine→TUI event channel.
    pub events_rx: mpsc::Receiver<CoreEvent>,
    /// Binary version string (e.g. `0.1.0`) shown by the welcome banner
    /// and `/version` command. Sourced from main's `CARGO_PKG_VERSION`.
    pub version: String,
    /// Abbreviated cwd shown by the welcome banner. `None` lets the App
    /// derive the value itself from `current_dir()`.
    pub cwd: Option<String>,
    /// Optional git branch surfaced in the bottom status bar.
    pub git_branch: Option<String>,
}

/// Entry point for the interactive TUI.
///
/// Runs a `tokio::select!` main loop over:
///   1. Terminal key events (via crossterm EventStream).
///   2. Engine events arriving on an mpsc channel.
///   3. A 100ms tick interval for spinner animation.
pub async fn run_tui(config: TuiConfig) -> cc_core::CcResult<()> {
    use crossterm::event::EventStream;

    // ── Channel: engine → TUI ─────────────────────────────────────────────
    // The channel was pre-created in main so that ChannelPrompter (which the
    // engine uses for permission prompts) shares the same sender.
    let events_tx = config.events_tx;
    let mut events_rx = config.events_rx;

    // Wire the events channel into the engine for streaming deltas / tool events.
    let engine = config.engine.with_events(events_tx.clone());

    // Wrap engine + message history in a shared mutex so the spawned turn
    // task can take exclusive access while the main loop owns the terminal.
    let engine_state: Arc<Mutex<(QueryEngine, Vec<MessageParam>)>> =
        Arc::new(Mutex::new((engine, config.messages)));

    let root_cancel = config.cancel;

    // ── Build TUI state ───────────────────────────────────────────────────
    let mut app = App::new(config.session_id.clone(), config.model.clone());
    // Load keybindings once at startup and stash them on the App so
    // `/reload-keybindings` can swap the map without restarting the TUI.
    // Edits to ~/.claude/keybindings.json are picked up on demand via the
    // slash command (there is no file watcher — see the optional task 3.1).
    app.keybindings = Keybindings::load();
    // Phase D metadata: version, cwd and git branch feed the welcome banner
    // and the bottom status bar. None of these change during a session, so
    // we set them once here.
    app.set_version(config.version);
    if let Some(cwd) = config.cwd {
        app.set_cwd(cwd);
    }
    app.set_git_branch(config.git_branch);
    let commands = config.commands;
    let cmd_ctx = config.command_ctx;
    let update_ctx = UpdateContext {
        commands: &commands,
        command_ctx: &cmd_ctx,
    };

    // ── Install panic hook before touching the terminal ──────────────────
    //
    // Under crossterm raw mode a panic rewrites the screen, garbles the
    // cursor, and throws the panic message into the void — the user sees a
    // corrupt terminal and has no idea what crashed. This hook writes the
    // full panic message + backtrace to `~/.claude/cc-tui-crash.log`
    // *before* letting the default hook fire, then drops raw mode so at
    // least stderr is readable by the parent shell. It's installed exactly
    // once per process via `OnceLock`.
    install_panic_hook();

    // ── Initialize terminal (inline viewport) ─────────────────────────────
    //
    // Phase D follow-up: switch from `ratatui::init()` (alt-screen) to an
    // inline viewport so the TUI behaves like the official Claude Code CLI:
    //   * content lives in the terminal's normal scrollback (no clear on
    //     enter, content remains visible after exit)
    //   * the viewport sits at the bottom of the terminal and grows with its
    //     content instead of always taking the whole screen
    //
    // We size the viewport to `App::estimate_viewport_rows(width)`, capped at
    // `terminal_height - 1`, and resize before each draw so the box visually
    // tracks the conversation.
    crossterm::terminal::enable_raw_mode()
        .map_err(|e| cc_core::CcError::Other(format!("enable raw mode: {e}")))?;
    let backend = CrosstermBackend::new(io::stdout());
    let term_size = crossterm::terminal::size()
        .map_err(|e| cc_core::CcError::Other(format!("terminal size: {e}")))?;
    let initial_height = clamp_viewport(app.estimate_viewport_rows(term_size.0), term_size.1);
    let mut terminal = Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Inline(initial_height),
        },
    )
    .map_err(|e| cc_core::CcError::Other(format!("terminal init: {e}")))?;

    // Current viewport state. Kept outside `draw_with_resize` so we can
    // resize *lazily* — only when the terminal itself changed size (SIGWINCH)
    // or when content actually needs another row. Without this gate, every
    // streamed token re-ran `estimate_viewport_rows`, resized the inline
    // viewport, and pushed the previous frame up into scrollback, producing
    // the "rendering flickers mid-chat" symptom the user reported.
    let mut vp = ViewportState {
        height: initial_height,
        term_size,
    };

    let mut reader = EventStream::new();
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(100));

    // Initial render.
    draw_with_resize(&mut terminal, &app, &mut vp)?;

    loop {
        let action: Option<AppAction> = tokio::select! {
            // Branch 1: terminal key events
            maybe_event = reader.next() => {
                match maybe_event {
                    Some(Ok(crossterm::event::Event::Key(key))) => {
                        map_key_event(&key, &app.keybindings, &app)
                    }
                    Some(Ok(crossterm::event::Event::Resize(_, _))) => None,
                    Some(Err(_)) | None => Some(AppAction::Quit),
                    _ => None,
                }
            }
            // Branch 2: engine events
            Some(event) = events_rx.recv() => {
                map_engine_event(event)
            }
            // Branch 3: tick (spinner animation)
            _ = tick.tick() => {
                Some(AppAction::Tick)
            }
        };

        if let Some(action) = action {
            let result = update(&mut app, action, &update_ctx);
            match result {
                UpdateResult::Quit => break,
                UpdateResult::ForceQuit => {
                    // Emergency exit: drop raw mode, cancel root token so any
                    // outstanding child tasks observe cancellation, and exit
                    // with SIGINT-style status. No alt-screen to leave under
                    // inline mode.
                    root_cancel.cancel();
                    let _ = crossterm::terminal::disable_raw_mode();
                    std::process::exit(130);
                }
                UpdateResult::SubmitToEngine(text) => {
                    // Spawn a background task for this engine turn.
                    let es = engine_state.clone();
                    let child_cancel = root_cancel.child_token();
                    app.current_turn_cancel = Some(child_cancel.clone());
                    let tx = events_tx.clone();
                    tokio::spawn(async move {
                        let mut guard = es.lock().await;
                        let (engine, messages) = &mut *guard;
                        if let Err(e) = engine.run_turn(text, |_| {}, messages, &child_cancel).await
                        {
                            // Send the error to the TUI (TurnComplete was not
                            // sent by the engine in this error path).
                            let _ = tx.send(CoreEvent::Error(e.to_string())).await;
                        }
                        // On success the engine already sent TurnComplete.
                    });
                }
                UpdateResult::Continue => {}
            }
        }

        // Redraw after every event, resizing the inline viewport only if
        // the terminal itself resized or content genuinely needs more rows.
        draw_with_resize(&mut terminal, &app, &mut vp)?;
    }

    // Restore terminal: drop raw mode but leave the rendered inline content
    // in scrollback. Insert a trailing newline so the user's next shell
    // prompt starts on a fresh row instead of overlapping our last line.
    let _ = crossterm::terminal::disable_raw_mode();
    println!();
    Ok(())
}

/// Cap a desired inline viewport height to `terminal_height - 1` so a row of
/// breathing room remains between the prior shell prompt and our top edge.
/// Floors at 4 so we never collapse below "input + footer" usability.
fn clamp_viewport(desired: u16, term_height: u16) -> u16 {
    let max = term_height.saturating_sub(1).max(4);
    desired.clamp(4, max)
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
fn install_panic_hook() {
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

/// Snapshot of the most recent inline viewport size + the terminal size it
/// was sized for. Held across draws so we resize only on real changes.
struct ViewportState {
    height: u16,
    term_size: (u16, u16),
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
fn draw_with_resize<B>(
    terminal: &mut Terminal<B>,
    app: &App,
    vp: &mut ViewportState,
) -> cc_core::CcResult<()>
where
    B: ratatui::backend::Backend,
{
    let term_size = crossterm::terminal::size()
        .map_err(|e| cc_core::CcError::Other(format!("terminal size: {e}")))?;
    let desired = app.estimate_viewport_rows(term_size.0);
    let capped = clamp_viewport(desired, term_size.1);

    let terminal_resized = term_size != vp.term_size;
    let needs_grow = capped > vp.height;

    let new_height = if terminal_resized {
        // Follow the terminal faithfully on real SIGWINCH, including shrinks.
        capped
    } else if needs_grow {
        capped
    } else {
        vp.height
    };

    if terminal_resized || new_height != vp.height {
        let _ = terminal.resize(Rect::new(0, 0, term_size.0, new_height));
        vp.height = new_height;
        vp.term_size = term_size;
    }

    terminal
        .draw(|frame| render::render(frame, app))
        .map_err(|e| cc_core::CcError::Other(format!("terminal draw error: {e}")))?;
    Ok(())
}

/// Map a crossterm key event to an AppAction based on keybindings and mode.
fn map_key_event(
    key: &crossterm::event::KeyEvent,
    kb: &Keybindings,
    app: &App,
) -> Option<AppAction> {
    use crossterm::event::{KeyCode, KeyModifiers};
    use keybindings::Action;

    // Check global keybindings first
    if let Some(action) = kb.match_action(key) {
        return match action {
            Action::Quit => Some(AppAction::Quit),
            Action::Abort => {
                // Second Ctrl+C within 2s → escalate to force quit so the user
                // always has a recovery path when the first abort is stalled.
                if app.within_force_quit_window(std::time::Instant::now()) {
                    Some(AppAction::ForceQuit)
                } else {
                    Some(AppAction::Abort)
                }
            }
            Action::Submit => Some(AppAction::Submit),
        };
    }

    // Mode-specific handling
    match app.mode {
        AppMode::CommandPalette => match key.code {
            KeyCode::Esc => Some(AppAction::PaletteCancel),
            KeyCode::Tab | KeyCode::Enter => Some(AppAction::PaletteAccept),
            KeyCode::Up => Some(AppAction::PaletteMove(-1)),
            KeyCode::Down => Some(AppAction::PaletteMove(1)),
            KeyCode::Char(c) => Some(AppAction::InsertChar(c)),
            KeyCode::Backspace => Some(AppAction::Backspace),
            _ => None,
        },
        AppMode::Input => match key.code {
            // Empty-buffer `/` opens the palette *and* inserts the char so
            // the filter starts at `/`. If the buffer is non-empty, treat
            // `/` as a regular character (matches M5 scope: picker triggers
            // "when buffer starts with `/`").
            KeyCode::Char('/') if app.input.is_empty() => Some(AppAction::PaletteOpen),
            KeyCode::Char(c) => Some(AppAction::InsertChar(c)),
            KeyCode::Backspace => Some(AppAction::Backspace),
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                Some(AppAction::NewLine)
            }
            KeyCode::PageUp => Some(AppAction::ScrollUp(10)),
            KeyCode::PageDown => Some(AppAction::ScrollDown(10)),
            KeyCode::Up if key.modifiers.contains(KeyModifiers::SHIFT) => {
                Some(AppAction::ScrollUp(3))
            }
            KeyCode::Down if key.modifiers.contains(KeyModifiers::SHIFT) => {
                Some(AppAction::ScrollDown(3))
            }
            _ => None,
        },
        AppMode::Streaming => match key.code {
            KeyCode::PageUp => Some(AppAction::ScrollUp(10)),
            KeyCode::PageDown => Some(AppAction::ScrollDown(10)),
            _ => None,
        },
        AppMode::PermissionPrompt => match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => Some(AppAction::PermissionAllow),
            KeyCode::Char('a') | KeyCode::Char('A') => Some(AppAction::PermissionAllowAlways),
            KeyCode::Char('n') | KeyCode::Char('N') => Some(AppAction::PermissionDeny),
            KeyCode::Esc => Some(AppAction::PermissionDeny),
            _ => None,
        },
    }
}

/// Map a `cc_core::AppEvent` from the engine to an `AppAction`.
fn map_engine_event(event: CoreEvent) -> Option<AppAction> {
    match event {
        CoreEvent::StreamDelta(text) => Some(AppAction::StreamDelta(text)),
        CoreEvent::ToolStart { name, input } => {
            let input_summary = summarize_input(&input);
            Some(AppAction::ToolStart {
                name,
                input_summary,
                raw_input: input,
            })
        }
        CoreEvent::ToolEnd { name, result } => {
            // Truncate very long tool output for display.
            let output = truncate_output(&result.content);
            Some(AppAction::ToolEnd {
                name,
                output,
                is_error: result.is_error,
            })
        }
        CoreEvent::TurnComplete { usage } => Some(AppAction::TurnComplete { usage }),
        CoreEvent::CompactBoundary => Some(AppAction::CompactBoundary),
        CoreEvent::PermissionRequest {
            id: _,
            tool_name,
            tool_input,
            response_tx,
        } => {
            let summary = summarize_input(&tool_input);
            Some(AppAction::ShowPermission {
                tool_name,
                summary,
                reply: response_tx,
            })
        }
        CoreEvent::Error(msg) => Some(AppAction::Error(msg)),
        // Ignored: thinking blocks and streaming tool-use fragments are
        // internal streaming details not shown in the transcript view.
        CoreEvent::StreamThinking(_)
        | CoreEvent::StreamToolUse(_)
        | CoreEvent::StreamEnd(_)
        | CoreEvent::TaskUpdate(_) => None,
    }
}

/// Return the longest prefix of `s` that fits in `max_bytes` **and** ends on
/// a UTF-8 char boundary. Using `&s[..max_bytes]` directly panics if byte
/// `max_bytes` lands inside a multi-byte codepoint — very common with CJK
/// (3 bytes/char) and emoji (4 bytes/char). Because the renderer runs under
/// crossterm raw mode, such a panic scrambles the terminal and the stderr
/// message is swallowed, making the TUI look like it "just died after a few
/// messages". This helper is the canonical truncation point for all
/// byte-bounded previews in this file.
fn truncate_at_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Format a JSON `Value` as a short one-line summary for the transcript.
fn summarize_input(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Object(map) => {
            // Pick the most informative field: command, path, pattern, query, url.
            for key in &["command", "path", "pattern", "query", "url", "content"] {
                if let Some(serde_json::Value::String(s)) = map.get(*key) {
                    let s = s.trim();
                    if s.len() > 120 {
                        return format!("{}…", truncate_at_char_boundary(s, 120));
                    }
                    return s.to_string();
                }
            }
            // Fall back to compact JSON, truncated.
            let s = serde_json::to_string(v).unwrap_or_default();
            if s.len() > 120 {
                format!("{}…", truncate_at_char_boundary(&s, 120))
            } else {
                s
            }
        }
        serde_json::Value::String(s) => {
            if s.len() > 120 {
                format!("{}…", truncate_at_char_boundary(s, 120))
            } else {
                s.clone()
            }
        }
        other => {
            let s = other.to_string();
            if s.len() > 120 {
                format!("{}…", truncate_at_char_boundary(&s, 120))
            } else {
                s
            }
        }
    }
}

/// Truncate long tool output to a preview for transcript display.
fn truncate_output(s: &str) -> String {
    const MAX_LINES: usize = 20;
    const MAX_BYTES: usize = 2000;
    let lines: Vec<&str> = s.lines().take(MAX_LINES + 1).collect();
    let truncated_lines = lines.len() > MAX_LINES;
    let joined = lines[..lines.len().min(MAX_LINES)].join("\n");
    if truncated_lines || joined.len() > MAX_BYTES {
        let preview = truncate_at_char_boundary(&joined, MAX_BYTES);
        format!("{preview}\n… (truncated)")
    } else {
        joined
    }
}

#[cfg(test)]
mod keymap_tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::time::{Duration, Instant};

    /// Second Ctrl+C within the 2s force-quit window must escalate to
    /// `AppAction::ForceQuit`, so users have an escape hatch when the first
    /// abort is still in flight.
    #[test]
    fn second_ctrl_c_within_window_escalates_to_force_quit() {
        let kb = Keybindings::default();
        let mut app = App::new("s".into(), "m".into());
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);

        // First press: ordinary Abort.
        let first = map_key_event(&ctrl_c, &kb, &app);
        assert!(
            matches!(first, Some(AppAction::Abort)),
            "first Ctrl+C: {first:?}"
        );

        // Stamp as the real update() handler would — within the window.
        app.last_abort_at = Some(Instant::now());

        let second = map_key_event(&ctrl_c, &kb, &app);
        assert!(
            matches!(second, Some(AppAction::ForceQuit)),
            "second Ctrl+C within window: {second:?}"
        );
    }

    /// A single Ctrl+C after the window has elapsed (>2s since the last abort
    /// stamp) MUST still map to a graceful `Abort`, not `ForceQuit`.
    #[test]
    fn single_ctrl_c_outside_window_still_aborts() {
        let kb = Keybindings::default();
        let mut app = App::new("s".into(), "m".into());
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);

        // Simulate a stale abort 3 seconds ago — past the 2s window.
        app.last_abort_at = Instant::now().checked_sub(Duration::from_secs(3));

        let action = map_key_event(&ctrl_c, &kb, &app);
        assert!(
            matches!(action, Some(AppAction::Abort)),
            "stale abort stamp must not promote to ForceQuit: {action:?}"
        );
    }

    /// First-ever Ctrl+C (no prior stamp) must be `Abort`, not `ForceQuit`.
    #[test]
    fn first_ctrl_c_with_no_prior_abort_is_graceful() {
        let kb = Keybindings::default();
        let app = App::new("s".into(), "m".into());
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let action = map_key_event(&ctrl_c, &kb, &app);
        assert!(matches!(action, Some(AppAction::Abort)), "got {action:?}");
    }
}

#[cfg(test)]
mod utf8_safety_tests {
    //! The TUI used to run `&s[..120]` / `&joined[..2000]` byte slicing on
    //! tool input summaries and tool output previews. A single Chinese/emoji
    //! character at the exact cutoff byte turned into a panic under
    //! crossterm raw mode, which is why chat sessions with CJK content
    //! "just died". These tests anchor the boundary-safe helpers so a
    //! future refactor can't reintroduce the panic silently.
    use super::*;
    use serde_json::json;

    #[test]
    fn truncate_at_char_boundary_never_splits_codepoints() {
        // 41 CJK chars × 3 bytes = 123 bytes — byte 120 lands inside the
        // 41st char. Direct slicing would panic.
        let s: String = "我".repeat(41);
        assert_eq!(s.len(), 123);
        let truncated = truncate_at_char_boundary(&s, 120);
        // 120 / 3 = 40 chars, 120 bytes exactly is a boundary.
        assert_eq!(truncated.chars().count(), 40);
        assert_eq!(truncated.len() % 3, 0);
    }

    #[test]
    fn truncate_at_char_boundary_backs_off_partial_codepoints() {
        // One 4-byte emoji: if we try to cut at 2 bytes we must back off to 0.
        let s = "🎉abc"; // emoji is 4 bytes
        let cut = truncate_at_char_boundary(s, 2);
        assert_eq!(cut, ""); // must back off before the emoji
        let cut4 = truncate_at_char_boundary(s, 4);
        assert_eq!(cut4, "🎉"); // exact boundary
        let cut5 = truncate_at_char_boundary(s, 5);
        assert_eq!(cut5, "🎉a"); // one ASCII after the emoji
    }

    #[test]
    fn truncate_at_char_boundary_returns_full_string_when_short() {
        assert_eq!(truncate_at_char_boundary("hi", 100), "hi");
        assert_eq!(truncate_at_char_boundary("", 100), "");
    }

    /// Regression: a long Chinese bash command used to crash the TUI the
    /// moment it arrived as a tool-start event.
    #[test]
    fn summarize_long_chinese_command_does_not_panic() {
        let long = "echo ".to_string() + &"测试中文命令超过一百二十字节的情况".repeat(10);
        // This would panic on the old byte-slicing path.
        let s = summarize_input(&json!({ "command": long }));
        assert!(s.ends_with("…"), "expected ellipsis suffix, got {s:?}");
    }

    /// Regression: tool output filled with emoji used to crash on the
    /// 2 KiB truncate path.
    #[test]
    fn truncate_output_with_emoji_does_not_panic() {
        let huge = "🎉".repeat(800); // ~3.2 KiB — above MAX_BYTES (2000)
        let s = truncate_output(&huge);
        assert!(s.contains("… (truncated)"), "got {s:?}");
    }

    /// Pure ASCII path is unchanged — a 200-byte command truncates at
    /// exactly 120 bytes + ellipsis.
    #[test]
    fn summarize_long_ascii_command_truncates_to_120_bytes() {
        let long = "a".repeat(200);
        let s = summarize_input(&json!({ "command": long }));
        assert!(s.ends_with("…"));
        // 120 'a's + one '…' (3-byte char).
        assert_eq!(s.chars().count(), 121);
    }
}

#[cfg(test)]
mod viewport_tests {
    //! Anchor the inline-viewport resize policy: grow on demand, follow
    //! SIGWINCH, but do NOT shrink per stream delta (which caused the
    //! mid-chat flicker).
    use super::*;

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
