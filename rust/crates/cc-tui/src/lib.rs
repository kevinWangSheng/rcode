//! cc-tui — interactive terminal UI for `claude`.
//!
//! Absorbs cc-commands (slash command registry) per Phase 2 Decision 3.
//! Provides Ratatui-based TUI with streaming render, permission dialogs,
//! and slash command dispatch.

pub mod action;
pub mod app;
pub mod commands;
pub mod event;
pub mod keybindings;
pub mod prompter;
pub mod render;

pub use action::{update, AppAction, UpdateContext, UpdateResult};
pub use app::{App, AppMode, PendingPermission, StatusLine, TranscriptItem};
pub use commands::{CommandContext, CommandOutcome, CommandRegistry};
pub use event::AppEvent;
pub use keybindings::Keybindings;
pub use prompter::ChannelPrompter;

use std::sync::Arc;

use cc_core::{AppEvent as CoreEvent, MessageParam};
use cc_query::QueryEngine;
use futures::StreamExt;
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
    let commands = config.commands;
    let cmd_ctx = config.command_ctx;
    let update_ctx = UpdateContext {
        commands: &commands,
        command_ctx: &cmd_ctx,
    };

    // ── Initialize terminal ───────────────────────────────────────────────
    let mut terminal = ratatui::init();
    let mut reader = EventStream::new();
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(100));

    // Initial render.
    terminal
        .draw(|frame| render::render(frame, &app))
        .map_err(|e| cc_core::CcError::Other(format!("terminal draw error: {e}")))?;

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
                    // Emergency exit: restore terminal state (disable raw
                    // mode, leave alternate screen), cancel root token so
                    // any outstanding child tasks observe cancellation, and
                    // exit with SIGINT-style status.
                    root_cancel.cancel();
                    ratatui::restore();
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

        // Redraw after every event.
        terminal
            .draw(|frame| render::render(frame, &app))
            .map_err(|e| cc_core::CcError::Other(format!("terminal draw error: {e}")))?;
    }

    // Restore terminal.
    ratatui::restore();
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
        AppMode::Input | AppMode::CommandPalette => match key.code {
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

/// Format a JSON `Value` as a short one-line summary for the transcript.
fn summarize_input(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Object(map) => {
            // Pick the most informative field: command, path, pattern, query, url.
            for key in &["command", "path", "pattern", "query", "url", "content"] {
                if let Some(serde_json::Value::String(s)) = map.get(*key) {
                    let s = s.trim();
                    if s.len() > 120 {
                        return format!("{}…", &s[..120]);
                    }
                    return s.to_string();
                }
            }
            // Fall back to compact JSON, truncated.
            let s = serde_json::to_string(v).unwrap_or_default();
            if s.len() > 120 {
                format!("{}…", &s[..120])
            } else {
                s
            }
        }
        serde_json::Value::String(s) => {
            if s.len() > 120 {
                format!("{}…", &s[..120])
            } else {
                s.clone()
            }
        }
        other => {
            let s = other.to_string();
            if s.len() > 120 {
                format!("{}…", &s[..120])
            } else {
                s
            }
        }
    }
}

/// Truncate long tool output to a preview for transcript display.
fn truncate_output(s: &str) -> String {
    const MAX_LINES: usize = 20;
    const MAX_CHARS: usize = 2000;
    let lines: Vec<&str> = s.lines().take(MAX_LINES + 1).collect();
    let truncated_lines = lines.len() > MAX_LINES;
    let joined = lines[..lines.len().min(MAX_LINES)].join("\n");
    if truncated_lines || joined.len() > MAX_CHARS {
        let preview = &joined[..joined.len().min(MAX_CHARS)];
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
