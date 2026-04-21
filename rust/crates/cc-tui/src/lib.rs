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

    // ── Print welcome banner to scrollback, BEFORE raw mode ──────────────
    //
    // The welcome banner is ~12 rows tall (fancy variant). Trying to render
    // it inside the fixed 8-row inline viewport just clips it. Instead we
    // emit it as a normal println! sequence so it sits in the terminal's
    // native scrollback above wherever our inline viewport ends up. User
    // scrolls up with mouse-wheel to see it; it also persists after exit
    // like any other CLI output.
    if app.is_empty_session() {
        let banner_width = crossterm::terminal::size().map(|s| s.0).unwrap_or(80);
        let tip_seed = app.session_started.elapsed().as_secs() / 30;
        for line in crate::welcome::render_welcome(banner_width, &app.version, &app.cwd, tip_seed) {
            // Strip ANSI styling for this path — we're writing directly to
            // stdout pre-raw-mode; ratatui's Line style attributes don't
            // survive the println! boundary cleanly. Plain text is fine
            // for the banner: version/cwd/tip readability is the goal.
            let plain: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            println!("{plain}");
        }
    }

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
    // Fixed inline viewport height. We no longer grow it with content:
    //
    //   - Streaming paragraphs are flushed to terminal scrollback the moment
    //     they cross a stable `\n\n` boundary (`flush_to_scrollback`), so
    //     `streaming_text` stays short and the viewport never needs to bulge
    //     to hold the whole response.
    //
    //   - Growing + later shrinking the inline viewport produced visual
    //     artefacts (user-reported empty-block below completed content,
    //     2026-04-20) because Ratatui's `Terminal::resize` interacts oddly
    //     with Inline viewport origin tracking after `insert_before` has
    //     shifted the viewport around.
    //
    // Keeping the viewport at a small, stable size sidesteps both problems
    // and matches what Claude Code's Ink TUI does: the live area is just
    // input + chrome, content lives in scrollback.
    const FIXED_INLINE_ROWS: u16 = 8;
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

/// Cap a desired inline viewport height to `terminal_height - 1` so a row of
/// breathing room remains between the prior shell prompt and our top edge.
/// Floors at 4 so we never collapse below "input + footer" usability.
fn clamp_viewport(desired: u16, term_height: u16) -> u16 {
    let max = term_height.saturating_sub(1).max(4);
    desired.clamp(4, max)
}

/// Append a one-line note to `~/.claude/cc-tui-crash.log` recording that
/// the inline-viewport init had to fall back to Fullscreen. We piggy-back
/// on the crash log rather than spawning a third file because users
/// already know to check that file when something looks wrong, and the
/// fallback is the kind of thing they'd want to see alongside crashes.
fn log_inline_fallback(reason: &str) -> std::io::Result<()> {
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
fn install_file_log() {
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

/// Active viewport flavour. `Fullscreen` is the fallback we land in when
/// the inline init's DSR-cursor probe times out (slow / nested terminals).
/// In Fullscreen mode, `flush_to_scrollback` and the lazy-resize logic are
/// no-ops because Ratatui's `Viewport::Fullscreen` doesn't support
/// `Terminal::insert_before` (it'd be a silent no-op anyway) and the area
/// is fixed to the terminal size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewportKind {
    Inline,
    Fullscreen,
}

/// Snapshot of the most recent inline viewport size + the terminal size it
/// was sized for. Held across draws so we resize only on real changes.
struct ViewportState {
    height: u16,
    term_size: (u16, u16),
    kind: ViewportKind,
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
            .draw(|frame| render::render(frame, app))
            .map_err(|e| cc_core::CcError::Other(format!("terminal draw error: {e}")))?;
        return Ok(());
    }

    let term_size = crossterm::terminal::size()
        .map_err(|e| cc_core::CcError::Other(format!("terminal size: {e}")))?;

    // Step 1: flush finalized transcript items to terminal scrollback before
    // we draw. After this returns, the viewport has *no* transcript items
    // to render — just live state (streaming text, spinner, input, chrome).
    flush_to_scrollback(terminal, app, term_size.0)?;

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
        .draw(|frame| render::render(frame, app))
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
///
/// Field-priority list mirrors the names used by the actual tool schemas
/// in `cc-tools/src/`:
///   - `command`           — Bash
///   - `file_path`         — Edit, Write, MultiEdit, Read
///   - `path`              — Glob (and any future tool that prefers `path`)
///   - `pattern`           — Grep, Glob
///   - `query`             — WebSearch
///   - `url`               — WebFetch
///   - `content`           — Write
///   - `prompt`            — TaskCreate, ApiSummarizer
///   - `name` / `title`    — TaskUpdate / generic
///
/// Adding `file_path` here was the user-visible fix for "Edit tool card
/// shows raw JSON" surfaced by the npcterm-driven smoke test. Without it,
/// every Edit/Write call rendered as `Edit({"file_path":"…","old_string":
/// "…",…})` instead of the cleaner `Edit(src/main.rs)`.
fn summarize_input(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Object(map) => {
            for key in &[
                "command",
                "file_path",
                "path",
                "pattern",
                "query",
                "url",
                "content",
                "prompt",
                "name",
                "title",
            ] {
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

    /// Regression for the npcterm-found bug: Edit/Write/MultiEdit use
    /// `file_path`, not `path`. Pre-fix the summary fell through to a
    /// raw JSON dump, so every Edit tool card looked like
    /// `Edit({"file_path":"…","old_string":"…",…})`.
    #[test]
    fn summarize_edit_uses_file_path_field() {
        let s = summarize_input(&json!({
            "file_path": "src/main.rs",
            "old_string": "hello",
            "new_string": "world"
        }));
        assert_eq!(s, "src/main.rs", "got {s:?}");
    }

    /// `path` (Glob) and `pattern` (Grep) keep working alongside file_path.
    #[test]
    fn summarize_path_and_pattern_still_work() {
        assert_eq!(summarize_input(&json!({ "path": "**/*.rs" })), "**/*.rs");
        assert_eq!(
            summarize_input(&json!({ "pattern": "fn main", "glob": "*.rs" })),
            "fn main"
        );
    }

    /// `prompt` (TaskCreate, ApiSummarizer) is now picked up too.
    #[test]
    fn summarize_picks_up_prompt_field() {
        assert_eq!(
            summarize_input(&json!({ "prompt": "summarise this file", "max_tokens": 200 })),
            "summarise this file"
        );
    }
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
