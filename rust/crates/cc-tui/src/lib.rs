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
mod runtime;
#[cfg(feature = "tui-syntect")]
pub mod syntax;
pub mod theme;
mod util;
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
use ratatui::{Terminal, TerminalOptions, Viewport};
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;

use runtime::{
    clamp_viewport, draw_with_resize, install_file_log, install_panic_hook, log_inline_fallback,
    ViewportKind, ViewportState, FIXED_INLINE_ROWS,
};
use util::{summarize_input, truncate_output};

/// Configuration for the TUI entry point.
pub struct TuiConfig {
    pub model: String,
    pub session_id: String,
    /// Fully-constructed engine (built by main with all tools, hooks, session).
    ///
    /// `None` means "demo / PTY-harness mode" — `Submit` from the user is
    /// swallowed with a system notice instead of spawning an engine turn, and
    /// all incoming events are assumed to originate from whoever holds
    /// `events_tx` (typically a scripted task). This is what lets us drive
    /// the full `run_tui` event loop from integration tests without a real
    /// `ApiClient` / API key.
    pub engine: Option<QueryEngine>,
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

    // Wire the events channel into the engine for streaming deltas / tool
    // events. `None` puts us in demo / PTY-harness mode: the event loop runs
    // normally but Submit never triggers a real engine turn.
    type EngineState = Arc<Mutex<(QueryEngine, Vec<MessageParam>)>>;
    let engine_state: Option<EngineState> = config.engine.map(|engine| {
        Arc::new(Mutex::new((
            engine.with_events(events_tx.clone()),
            config.messages,
        )))
    });

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

    // Probe/test hook: when `CC_TUI_DEMO_AUTO_STREAM=1` is set, start the
    // app already in Streaming mode so scripted `StreamDelta` events fed
    // over the channel are accepted immediately. Without this, the demo
    // binary's events are all dropped by the `AppAction::StreamDelta`
    // guard (which correctly refuses tokens when mode is Input — that's
    // the AC-2b abort-safety invariant). Production clients do not set
    // this variable; the engine flips mode to Streaming on user submit
    // like normal.
    if std::env::var_os("CC_TUI_DEMO_AUTO_STREAM").is_some() {
        app.start_stream();
    }

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

    // ── Route tracing to a file if requested ─────────────────────────────
    //
    // Under raw mode we can't print tracing to stderr without corrupting
    // the screen. If the caller set `CC_TUI_LOG_FILE=/path/to/log`, route
    // all `tracing` events to that file instead. Harness tests pipe this
    // to a temp file they then read + assert on. Idempotent per process.
    install_file_log();

    // ── Initialize terminal (inline viewport) ─────────────────────────────
    //
    // Phase D follow-up: switch from `ratatui::init()` (alt-screen) to an
    // inline viewport so the TUI behaves like the official Claude Code CLI:
    //   * content lives in the terminal's normal scrollback (no clear on
    //     enter, content remains visible after exit)
    //   * the viewport sits at a fixed small size at the bottom; streaming
    //     paragraphs flush into scrollback via `insert_before` to keep the
    //     viewport payload short.
    crossterm::terminal::enable_raw_mode()
        .map_err(|e| cc_core::CcError::Other(format!("enable raw mode: {e}")))?;
    let term_size = crossterm::terminal::size()
        .map_err(|e| cc_core::CcError::Other(format!("terminal size: {e}")))?;
    let initial_height = clamp_viewport(FIXED_INLINE_ROWS, term_size.1);

    // `Viewport::Inline` queries the terminal cursor position via DSR
    // (`ESC[6n`) at init time. Some emulators answer too slowly (>2 s
    // crossterm timeout), or in some setups (nested tmux, slow ssh
    // pipelines, certain headless emulators) the response gets eaten
    // entirely. Surfacing that as "terminal init: cursor position could
    // not be read" was the symptom this fallback fixes — the user got an
    // unhelpful error and had to bail. Now we silently fall back to
    // Fullscreen, which works everywhere; the only visible loss is that
    // history doesn't flow into terminal scrollback (the user instead
    // scrolls inside the app via PageUp). We log a one-line warning to
    // both the tracing log and the crash log so the user can opt to
    // re-run on a faster terminal if they want full inline behaviour.
    // Escape hatch for users who know their terminal is slow to answer DSR
    // (nested tmux, screen, some ssh setups, npcterm) and don't want to
    // wait the 2 s crossterm cursor-query timeout on every launch. Setting
    // `CC_TUI_FORCE_FULLSCREEN=1` skips the Inline attempt entirely. They
    // lose `insert_before`-to-scrollback (same loss as the automatic
    // fallback), gain a near-instant startup.
    let force_fullscreen = std::env::var_os("CC_TUI_FORCE_FULLSCREEN").is_some();

    let inline_attempt = if force_fullscreen {
        Err(std::io::Error::other(
            "CC_TUI_FORCE_FULLSCREEN=1 set; skipping Inline",
        ))
    } else {
        let backend = CrosstermBackend::new(io::stdout());
        Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Inline(initial_height),
            },
        )
    };
    let (mut terminal, viewport_kind) = match inline_attempt {
        Ok(t) => (t, ViewportKind::Inline),
        Err(e) => {
            tracing::warn!(
                "Inline viewport init failed ({e}); falling back to Fullscreen. \
                 Terminal scrollback inheritance disabled — use PageUp/PageDown \
                 inside the app to see history."
            );
            let _ = log_inline_fallback(&format!("{e}"));
            let backend = CrosstermBackend::new(io::stdout());
            let t = Terminal::with_options(
                backend,
                TerminalOptions {
                    viewport: Viewport::Fullscreen,
                },
            )
            .map_err(|e| cc_core::CcError::Other(format!("terminal init: {e}")))?;
            (t, ViewportKind::Fullscreen)
        }
    };

    // In Fullscreen fallback we own the whole terminal but did NOT enter
    // alt-screen (we want history to remain visible after exit). Without
    // this clear the prior shell prompt + any output stays painted under
    // the empty rows of our viewport, so the user sees our chrome
    // overlapping the previous output. `Terminal::clear` blanks the whole
    // viewport in one operation. Skipped under Inline because there's no
    // overlap risk — Inline only paints its own carved region.
    if viewport_kind == ViewportKind::Fullscreen {
        let _ = terminal.clear();
    }

    // ── Welcome banner (empty-session only) ──────────────────────────────
    //
    // The banner sits above any assistant output. We render it through
    // two different paths depending on the viewport we ended up with:
    //
    //   Inline  → push into native terminal scrollback via
    //             `Terminal::insert_before`. The banner survives across
    //             the whole session and the user can scroll the terminal
    //             up to see it later.
    //   Fullscreen → stash on `App`; `render_transcript` paints it into
    //             the transcript area for as long as `is_empty_session()`
    //             holds. Once the user submits a turn, transcript items
    //             take over naturally.
    //
    // We deliberately do NOT println! the banner before raw mode like
    // the earlier implementation did: the Fullscreen fallback calls
    // `terminal.clear()` right above this block, which would wipe a
    // pre-printed banner off the screen.
    if app.is_empty_session() {
        let banner_width = term_size.0;
        let tip_seed = app.session_started.elapsed().as_secs() / 30;
        let lines =
            crate::welcome::render_welcome(banner_width, &app.version, &app.cwd, tip_seed);
        match viewport_kind {
            ViewportKind::Inline => {
                let n = lines.len() as u16;
                let lines_for_closure = lines.clone();
                if let Err(e) = terminal.insert_before(n, |buf| {
                    use ratatui::widgets::{Paragraph, Widget};
                    let area = buf.area;
                    Paragraph::new(lines_for_closure).render(area, buf);
                }) {
                    tracing::warn!("welcome insert_before failed: {e}");
                }
            }
            ViewportKind::Fullscreen => {
                app.set_welcome_banner(lines);
            }
        }
    }

    // Current viewport state. Kept outside `draw_with_resize` so we can
    // resize *lazily* — only when the terminal itself changed size (SIGWINCH)
    // or when content actually needs another row. Without this gate, every
    // streamed token re-ran `estimate_viewport_rows`, resized the inline
    // viewport, and pushed the previous frame up into scrollback, producing
    // the "rendering flickers mid-chat" symptom the user reported.
    let mut vp = ViewportState {
        height: initial_height,
        term_size,
        kind: viewport_kind,
    };

    let mut reader = EventStream::new();
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(100));

    // Initial render.
    draw_with_resize(&mut terminal, &mut app, &mut vp)?;

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
                    // Spawn a background task for this engine turn — but only
                    // if we have a real engine. In demo / PTY-harness mode
                    // (`engine_state.is_none()`) we drop the submit on the
                    // floor and let the scripted event source drive the TUI
                    // instead, so the rest of the event loop keeps exercising
                    // every code path except the real network turn.
                    if let Some(es) = engine_state.as_ref() {
                        let es = es.clone();
                        let child_cancel = root_cancel.child_token();
                        app.current_turn_cancel = Some(child_cancel.clone());
                        let tx = events_tx.clone();
                        tokio::spawn(async move {
                            let mut guard = es.lock().await;
                            let (engine, messages) = &mut *guard;
                            if let Err(e) =
                                engine.run_turn(text, |_| {}, messages, &child_cancel).await
                            {
                                // Send the error to the TUI (TurnComplete was not
                                // sent by the engine in this error path).
                                let _ = tx.send(CoreEvent::Error(e.to_string())).await;
                            }
                            // On success the engine already sent TurnComplete.
                        });
                    } else {
                        // Demo mode — synthesize a quick TurnComplete so the
                        // mode returns to Input without freezing on
                        // `AppMode::Streaming`.
                        let tx = events_tx.clone();
                        tokio::spawn(async move {
                            let _ = tx
                                .send(CoreEvent::TurnComplete {
                                    usage: cc_core::Usage::default(),
                                })
                                .await;
                            drop(text);
                        });
                    }
                }
                UpdateResult::Continue => {}
            }
        }

        // Redraw after every event, resizing the inline viewport only if
        // the terminal itself resized or content genuinely needs more rows.
        draw_with_resize(&mut terminal, &mut app, &mut vp)?;
    }

    // Restore terminal: drop raw mode but leave the rendered inline content
    // in scrollback. Insert a trailing newline so the user's next shell
    // prompt starts on a fresh row instead of overlapping our last line.
    let _ = crossterm::terminal::disable_raw_mode();
    println!();
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
            KeyCode::Left => Some(AppAction::CursorMove(-1)),
            KeyCode::Right => Some(AppAction::CursorMove(1)),
            KeyCode::Home => Some(AppAction::CursorHome),
            KeyCode::End => Some(AppAction::CursorEnd),
            KeyCode::Delete => Some(AppAction::DeleteChar),
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
            // `?` on empty input runs /help (matches the help footer hint).
            // When the buffer is non-empty, `?` is a normal char.
            KeyCode::Char('?') if app.input.is_empty() => Some(AppAction::HelpShortcut),
            // `@` anywhere in the buffer opens the file picker so the
            // user can attach a path mid-prose without having to
            // start from scratch. The palette closes on Esc (restore)
            // or Tab/Enter (insert selected path at cursor).
            KeyCode::Char('@') => Some(AppAction::FilePaletteOpen),
            KeyCode::Char(c) => Some(AppAction::InsertChar(c)),
            KeyCode::Backspace => Some(AppAction::Backspace),
            KeyCode::Delete => Some(AppAction::DeleteChar),
            KeyCode::Left => Some(AppAction::CursorMove(-1)),
            KeyCode::Right => Some(AppAction::CursorMove(1)),
            KeyCode::Home => Some(AppAction::CursorHome),
            KeyCode::End => Some(AppAction::CursorEnd),
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
            // Bare Up/Down (no modifiers) walk the input-history ring
            // — bash/zsh convention. Shift+Up/Down above keeps the
            // transcript-scroll binding.
            KeyCode::Up => Some(AppAction::HistoryPrev),
            KeyCode::Down => Some(AppAction::HistoryNext),
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
