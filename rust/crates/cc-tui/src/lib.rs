//! cc-tui — interactive terminal UI for `claude`.
//!
//! Top-level entry point is [`run_tui`]. The architecture is:
//!
//! ```text
//!  ┌─────────────────┐    AppEvent     ┌──────────────────┐
//!  │ crossterm input │ ──────────────▶ │  main loop       │
//!  │ thread          │                 │  (this module)   │
//!  └─────────────────┘                 │                  │
//!                                      │  - renders App   │
//!  ┌─────────────────┐    AppEvent     │  - dispatches    │
//!  │ engine task     │ ──────────────▶ │    commands      │
//!  │ (run_turn)      │                 │  - manages perm  │
//!  └────────┬────────┘                 │    dialog        │
//!           ▲                          └────────┬─────────┘
//!           │ PromptDecision (oneshot)          │
//!           └───────────────────────────────────┘
//! ```
//!
//! The engine knows nothing about the TUI — it talks to a [`prompter::ChannelPrompter`]
//! that hides everything behind a `PermissionPrompter` trait.

pub mod app;
pub mod event;
pub mod keybindings;
pub mod prompter;
pub mod render;

use std::io;
use std::sync::Arc;
use std::time::Duration;

use crossterm::{
    event::{
        self as crossterm_event, DisableMouseCapture, EnableMouseCapture, Event, KeyCode,
        KeyEvent, KeyEventKind, KeyModifiers,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use tokio::sync::{mpsc, oneshot};

use std::path::PathBuf;

use cc_api::ApiClient;
use cc_commands::{parse as parse_cmd, CommandContext, CommandOutcome, CommandRegistry};
use cc_config::expand_model_alias;
use cc_core::{MessageParam, SystemBlock};
use cc_hooks::HookRunner;
use cc_permissions::PermissionEngine;
use cc_query::{
    compact_messages, permission_prompt::summarize_input, PromptDecision, QueryEngine,
    QueryOptions,
};
use cc_session::Session;
use cc_tools::Tool;

use crate::app::{App, PendingPermission, StreamState};
use crate::event::AppEvent;
use crate::keybindings::{Action, Keybindings};
use crate::prompter::ChannelPrompter;

/// Construction inputs for the TUI. The caller (the `cc` binary) builds the
/// pieces — API client, tools, permissions, hooks, system blocks — and hands
/// them to us. We own the run loop and lifecycle from here on out.
pub struct TuiConfig {
    pub api: ApiClient,
    pub tools: Vec<Arc<dyn Tool>>,
    pub permissions: PermissionEngine,
    pub hooks: HookRunner,
    pub session: Session,
    pub initial_messages: Vec<MessageParam>,
    pub system_blocks: Vec<SystemBlock>,
    pub options: QueryOptions,
    pub version: String,
    /// MCP server names from settings (e.g. `filesystem`, `github`). Surfaced
    /// in `/mcp` and `/config`. Empty when no servers are configured.
    pub mcp_server_names: Vec<String>,
    /// Project root directory. Used by `/init` to write `CLAUDE.md` and by
    /// `/config` to display location. `None` if the binary was launched from
    /// somewhere we couldn't determine a project root.
    pub project_root: Option<PathBuf>,
}

/// Entry point — run the interactive TUI until the user quits.
///
/// Sets up the terminal in raw mode, enters the alternate screen, runs the main
/// loop, then unconditionally restores the terminal on exit (success or panic
/// path; see [`restore_terminal`]).
pub async fn run_tui(cfg: TuiConfig) -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Drive the main loop. Errors here still need to clean up the terminal.
    let result = run_loop(&mut terminal, cfg).await;

    restore_terminal(&mut terminal).ok();
    result
}

fn restore_terminal<B: ratatui::backend::Backend + std::io::Write>(
    terminal: &mut Terminal<B>,
) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;
    Ok(())
}

async fn run_loop<B: ratatui::backend::Backend + std::io::Write>(
    terminal: &mut Terminal<B>,
    cfg: TuiConfig,
) -> io::Result<()> {
    let TuiConfig {
        api,
        tools,
        permissions,
        hooks,
        session,
        mut initial_messages,
        system_blocks,
        options,
        version,
        mcp_server_names,
        project_root,
    } = cfg;
    let hook_event_names = hooks.event_names();

    // Discover slash commands. Loaded once at startup; user can re-discover by
    // restarting the TUI. (Hot-reload is out of scope for M3.)
    let registry = CommandRegistry::discover();

    // Channels:
    //   event_tx/rx: every input that drives the main loop
    //   engine commands: oneshot reply for permission requests; engine_done sentinel
    let (event_tx, mut event_rx) = mpsc::channel::<AppEvent>(256);

    // Spawn the blocking input reader on a dedicated OS thread.
    spawn_input_thread(event_tx.clone());

    // Initialize app + render the welcome banner.
    let mut app = App::new(session.id.clone());
    seed_initial_transcript(&mut app, &initial_messages);
    if !registry.skills.is_empty() {
        app.push_system(format!(
            "Loaded {} skill(s). Type /help to list available commands.",
            registry.skills.len()
        ));
    }
    app.push_system(registry.help_text());

    let kb = Keybindings::load();

    // Build the engine. We'll only ever own one — re-using it across turns
    // preserves session state inside the engine itself.
    let prompter: Arc<dyn cc_query::PermissionPrompter> =
        Arc::new(ChannelPrompter::new(event_tx.clone()));
    let mut engine = Some(
        QueryEngine::new(
            api,
            tools,
            permissions,
            hooks,
            session,
            system_blocks,
            options,
        )
        .with_prompter(prompter),
    );

    // State for the currently-running engine task. When `engine_handle` is `Some`,
    // a turn is in progress. The handle returns the engine + final result so we
    // can re-use it for the next turn.
    let mut engine_handle: Option<tokio::task::JoinHandle<EngineTaskResult>> = None;
    // Pending permission reply channel — set when we're showing a modal.
    let mut pending_reply: Option<oneshot::Sender<PromptDecision>> = None;
    let mut should_quit = false;

    // Initial draw.
    terminal.draw(|f| render::render(f, &app))?;

    while !should_quit {
        let ev = match tokio::time::timeout(Duration::from_millis(100), event_rx.recv()).await {
            Ok(Some(ev)) => ev,
            Ok(None) => break, // all senders dropped
            Err(_) => AppEvent::Tick,
        };

        match ev {
            AppEvent::Tick => {}

            AppEvent::Key(key) => {
                if key.kind == KeyEventKind::Release {
                    // Ignore key-release on platforms that emit them.
                } else if app.permission.is_some() {
                    handle_permission_key(&mut app, &mut pending_reply, &key);
                } else if let Some(action) = kb.match_action(&key) {
                    match action {
                        Action::Quit => should_quit = true,
                        Action::Abort => {
                            // Cancel a running engine task by aborting its handle.
                            if let Some(h) = engine_handle.take() {
                                h.abort();
                                app.abort_stream();
                                app.status =
                                    "Stream aborted (Ctrl+C). Press Enter to send another message.".into();
                                // Engine task is gone — we cannot reuse the engine it owned.
                                // For M3 we recreate it on the next turn from scratch by re-using
                                // the JoinHandle's last-known state. The simpler approach: keep the
                                // engine in `engine` slot only when no task is running.
                                engine = None;
                                app.status += "  (engine reset — session id is unchanged)";
                            }
                        }
                        Action::Submit => {
                            // Build a fresh CommandContext each turn so /model
                            // and /config reflect any in-place mutation.
                            let cmd_ctx = CommandContext {
                                version: version.clone(),
                                model: engine
                                    .as_ref()
                                    .map(|e| e.model().to_string())
                                    .unwrap_or_else(|| "(engine reset)".into()),
                                mcp_servers: mcp_server_names.clone(),
                                hook_events: hook_event_names.clone(),
                                project_root: project_root.clone(),
                            };
                            handle_submit(
                                &mut app,
                                &registry,
                                &cmd_ctx,
                                &event_tx,
                                &mut engine,
                                &mut engine_handle,
                                &mut initial_messages,
                                &mut should_quit,
                            );
                        }
                    }
                } else {
                    handle_text_key(&mut app, &key);
                }
            }

            AppEvent::Token(delta) => {
                app.on_token(&delta);
            }

            AppEvent::EngineDone(result) => {
                let handle = engine_handle.take();
                let engine_back = if let Some(h) = handle {
                    match h.await {
                        Ok(EngineTaskResult { engine, .. }) => Some(engine),
                        Err(_) => None,
                    }
                } else {
                    None
                };
                engine = engine_back;

                // If auto-compact fired during this turn, surface a marker in
                // the transcript before we commit the streaming assistant
                // text — chronologically compaction happened mid-turn, but
                // rendering the boundary right before the final assistant
                // block gives the user a clear visual cue that context was
                // trimmed as part of what they're looking at.
                if engine
                    .as_ref()
                    .is_some_and(|e| e.compacted_last_turn())
                {
                    app.push_compact_boundary();
                }

                match result {
                    Ok(_) => {
                        app.finish_stream();
                        app.status = "Done. Type your next message or /help.".into();
                    }
                    Err(e) => {
                        app.finish_stream();
                        app.push_system(format!("Engine error: {e}"));
                        app.status = "Engine error — see transcript.".into();
                    }
                }

                // Process queued user inputs (submitted while the stream was running).
                if let Some(next) = app.queued.pop_front() {
                    submit_user_text(
                        &mut app,
                        next,
                        &event_tx,
                        &mut engine,
                        &mut engine_handle,
                        &mut initial_messages,
                    );
                }
            }

            AppEvent::PermissionRequest {
                tool_name,
                input,
                reply,
            } => {
                let summary = summarize_input(&tool_name, &input);
                app.permission = Some(PendingPermission {
                    tool_name: tool_name.clone(),
                    summary,
                });
                pending_reply = Some(reply);
                app.status = format!("Permission required for {tool_name}. Press y / a / n.");
            }

            AppEvent::Abort => {
                // Coalesced abort from a key chord matched by kb.match_action above —
                // we already handle it under Key. This branch exists for symmetry
                // and future programmatic aborts.
            }

            AppEvent::Quit => {
                should_quit = true;
            }
        }

        terminal.draw(|f| render::render(f, &app))?;
    }

    // Drain any in-flight engine task on shutdown so we don't leak it.
    if let Some(h) = engine_handle.take() {
        h.abort();
        let _ = h.await;
    }

    Ok(())
}

/// Result returned from the engine task to the main loop. Carries the engine
/// itself back so we can re-use it for the next turn.
struct EngineTaskResult {
    engine: QueryEngine,
}

fn spawn_input_thread(tx: mpsc::Sender<AppEvent>) {
    std::thread::spawn(move || loop {
        match crossterm_event::poll(Duration::from_millis(50)) {
            Ok(true) => match crossterm_event::read() {
                Ok(Event::Key(key)) => {
                    if tx.blocking_send(AppEvent::Key(key)).is_err() {
                        return;
                    }
                }
                Ok(_) => {} // resize, mouse, etc — ignore for now
                Err(_) => return,
            },
            Ok(false) => {} // timeout, loop again
            Err(_) => return,
        }
    });
}

fn seed_initial_transcript(app: &mut App, messages: &[MessageParam]) {
    use cc_core::{ContentBlock, MessageContent, Role};
    for m in messages {
        let text = match &m.content {
            MessageContent::Text(t) => t.clone(),
            MessageContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text(t) => Some(t.text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        };
        if text.is_empty() {
            continue;
        }
        match m.role {
            Role::User => app.push_user(text),
            Role::Assistant => app.transcript.push(crate::app::TranscriptItem::Assistant(text)),
        }
    }
}

fn handle_text_key(app: &mut App, key: &KeyEvent) {
    match key.code {
        KeyCode::Backspace => {
            app.input.pop();
        }
        KeyCode::Char(c) => {
            // Plain printable input — also accept SHIFT+char.
            if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT {
                app.input.push(c);
            }
        }
        KeyCode::PageUp => {
            app.scroll = app.scroll.saturating_add(5);
        }
        KeyCode::PageDown => {
            app.scroll = app.scroll.saturating_sub(5);
        }
        _ => {}
    }
}

fn handle_permission_key(
    app: &mut App,
    pending_reply: &mut Option<oneshot::Sender<PromptDecision>>,
    key: &KeyEvent,
) {
    let decision = match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => Some(PromptDecision::Allow),
        KeyCode::Char('a') | KeyCode::Char('A') => Some(PromptDecision::AllowAlways),
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Some(PromptDecision::Deny),
        _ => None,
    };
    let Some(d) = decision else { return };

    if let Some(reply) = pending_reply.take() {
        let _ = reply.send(d);
    }
    let tool = app
        .permission
        .as_ref()
        .map(|p| p.tool_name.clone())
        .unwrap_or_default();
    app.permission = None;
    app.status = match d {
        PromptDecision::Allow => format!("Allowed {tool} once."),
        PromptDecision::AllowAlways => format!("Allowed {tool} for the rest of this session."),
        PromptDecision::Deny => format!("Denied {tool}."),
    };
}

#[allow(clippy::too_many_arguments)]
fn handle_submit(
    app: &mut App,
    registry: &CommandRegistry,
    cmd_ctx: &CommandContext,
    event_tx: &mpsc::Sender<AppEvent>,
    engine_slot: &mut Option<QueryEngine>,
    engine_handle: &mut Option<tokio::task::JoinHandle<EngineTaskResult>>,
    messages: &mut Vec<MessageParam>,
    should_quit: &mut bool,
) {
    let text = std::mem::take(&mut app.input);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }

    // Slash command path.
    if let Some(cmd) = parse_cmd(trimmed) {
        match registry.execute(&cmd, cmd_ctx) {
            CommandOutcome::Info(text) => {
                app.push_system(text);
            }
            CommandOutcome::SubmitUserMessage(rendered) => {
                submit_user_text(app, rendered, event_tx, engine_slot, engine_handle, messages);
            }
            CommandOutcome::Clear => {
                app.transcript.clear();
                app.streaming_text.clear();
                app.push_system("Transcript cleared. Session is unchanged.".into());
            }
            CommandOutcome::Exit => {
                *should_quit = true;
            }
            CommandOutcome::Compact => {
                let before = messages.len();
                compact_messages(messages);
                let after = messages.len();
                app.push_system(format!(
                    "Compacted transcript: {before} → {after} messages."
                ));
            }
            CommandOutcome::SwitchModel(name) => {
                let resolved = expand_model_alias(&name);
                if let Some(engine) = engine_slot.as_mut() {
                    engine.set_model(resolved.clone());
                    app.push_system(format!("Switched model to {resolved}."));
                } else {
                    app.push_system(format!(
                        "Engine is not available (was reset). Cannot switch model right now. \
                         Restart claude with --model {resolved}."
                    ));
                }
            }
            CommandOutcome::Unknown(msg) => {
                app.push_system(msg);
            }
        }
        return;
    }

    submit_user_text(app, text, event_tx, engine_slot, engine_handle, messages);
}

fn submit_user_text(
    app: &mut App,
    text: String,
    event_tx: &mpsc::Sender<AppEvent>,
    engine_slot: &mut Option<QueryEngine>,
    engine_handle: &mut Option<tokio::task::JoinHandle<EngineTaskResult>>,
    messages: &mut Vec<MessageParam>,
) {
    // While a stream is in flight, queue input rather than starting a second turn.
    if app.stream_state != StreamState::Idle || engine_handle.is_some() {
        app.queued.push_back(text.clone());
        app.status = format!("Queued ({} pending). Will run after current turn.", app.queued.len());
        return;
    }

    let engine = match engine_slot.take() {
        Some(e) => e,
        None => {
            app.push_system("Engine is not available (was reset). Restart claude to recover.".into());
            return;
        }
    };

    app.push_user(text.clone());
    app.start_stream();
    app.status = "Streaming...".into();

    let tx = event_tx.clone();
    let mut messages_clone = messages.clone();
    let handle = tokio::spawn(async move {
        let mut engine = engine;
        let result = engine
            .run_turn(text, |delta| {
                let _ = tx.try_send(AppEvent::Token(delta.to_string()));
            }, &mut messages_clone)
            .await;
        let send_result = match &result {
            Ok(s) => Ok(s.clone()),
            Err(e) => Err(e.to_string()),
        };
        let _ = tx.send(AppEvent::EngineDone(send_result)).await;
        EngineTaskResult { engine }
    });
    *engine_handle = Some(handle);
    // The cloned `messages_clone` will be lost when the task ends — for M3 we
    // re-derive history from the session JSONL on resume rather than threading
    // it back here. This keeps the channel surface narrow.
    let _ = messages;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::TranscriptItem;

    #[test]
    fn keybinding_chord_round_trip() {
        let kb = Keybindings::default();
        let key = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL);
        assert_eq!(kb.match_action(&key), Some(Action::Quit));
    }

    /// Mirrors the ordering in the `AppEvent::EngineDone` branch: when the
    /// engine signals `compacted_last_turn`, the TUI pushes a compact boundary
    /// and then commits the streaming assistant text. The transcript should
    /// show the boundary before the assistant reply for that turn.
    #[test]
    fn compact_boundary_rendered_before_streamed_reply() {
        use crate::app::App;

        let mut app = App::new("sess".into());
        app.push_user("long history please".into());
        app.start_stream();
        app.on_token("ok");

        // Simulate the EngineDone path: (1) engine says compacted, (2)
        // boundary pushed, (3) stream finishes and assistant text lands in the
        // transcript.
        app.push_compact_boundary();
        app.finish_stream();

        // Transcript order: User, CompactBoundary, Assistant.
        assert!(matches!(app.transcript[0], TranscriptItem::User(_)));
        assert!(matches!(app.transcript[1], TranscriptItem::CompactBoundary));
        assert!(matches!(app.transcript[2], TranscriptItem::Assistant(ref t) if t == "ok"));
    }
}
