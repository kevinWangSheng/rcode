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

/// Configuration for the TUI entry point.
pub struct TuiConfig {
    pub model: String,
    pub session_id: String,
}

/// Entry point for the interactive TUI.
///
/// Runs a `tokio::select!` main loop over:
///   1. Terminal key events (via crossterm EventStream).
///   2. Engine events arriving on an mpsc channel.
///   3. A 100ms tick interval for spinner animation.
pub async fn run_tui(config: TuiConfig) -> cc_core::CcResult<()> {
    use crossterm::event::EventStream;
    use futures::StreamExt;
    use tokio::sync::mpsc;

    let mut app = App::new(config.session_id.clone(), config.model.clone());
    let kb = Keybindings::load();
    let commands = CommandRegistry::default();
    let cmd_ctx = commands::CommandContext::new(env!("CARGO_PKG_VERSION"), &config.model);

    // Channel for engine → TUI events. The engine task (spawned on Submit)
    // sends AppEvents here; the main loop converts them to AppActions.
    let (_engine_tx, mut engine_rx) = mpsc::unbounded_channel::<AppEvent>();

    // Initialize terminal
    let mut terminal = ratatui::init();

    // Crossterm event stream
    let mut reader = EventStream::new();

    // Tick interval
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(100));

    let update_ctx = UpdateContext {
        commands: &commands,
        command_ctx: &cmd_ctx,
    };

    // Initial render
    terminal
        .draw(|frame| render::render(frame, &app))
        .map_err(|e| cc_core::CcError::Other(format!("terminal draw error: {e}")))?;

    loop {
        let action: Option<AppAction> = tokio::select! {
            // Branch 1: terminal key events
            maybe_event = reader.next() => {
                match maybe_event {
                    Some(Ok(crossterm::event::Event::Key(key))) => {
                        map_key_event(&key, &kb, &app)
                    }
                    Some(Ok(crossterm::event::Event::Resize(_, _))) => {
                        // Just redraw
                        None
                    }
                    Some(Err(_)) | None => {
                        // Terminal closed or error — quit
                        Some(AppAction::Quit)
                    }
                    _ => None,
                }
            }
            // Branch 2: engine events
            Some(event) = engine_rx.recv() => {
                map_engine_event(event)
            }
            // Branch 3: tick
            _ = tick.tick() => {
                Some(AppAction::Tick)
            }
        };

        if let Some(action) = action {
            let result = update(&mut app, action, &update_ctx);
            if result == UpdateResult::Quit {
                break;
            }
        }

        // Redraw
        terminal
            .draw(|frame| render::render(frame, &app))
            .map_err(|e| cc_core::CcError::Other(format!("terminal draw error: {e}")))?;
    }

    // Restore terminal
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
            Action::Abort => Some(AppAction::Abort),
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

/// Map an engine event to an AppAction.
fn map_engine_event(event: AppEvent) -> Option<AppAction> {
    match event {
        AppEvent::Token(delta) => Some(AppAction::StreamDelta(delta)),
        AppEvent::ToolStart { name, input_summary } => {
            Some(AppAction::ToolStart { name, input_summary })
        }
        AppEvent::ToolEnd { name, output, is_error } => {
            Some(AppAction::ToolEnd { name, output, is_error })
        }
        AppEvent::EngineDone(Ok(_)) => Some(AppAction::StreamEnd),
        AppEvent::EngineDone(Err(msg)) => Some(AppAction::Error(msg)),
        AppEvent::TurnComplete { usage } => Some(AppAction::TurnComplete { usage }),
        AppEvent::PermissionRequest { tool_name, input, reply } => {
            let summary = format!("{}: {}", tool_name, serde_json::to_string(&input).unwrap_or_default());
            Some(AppAction::ShowPermission { tool_name, summary, reply })
        }
        AppEvent::Abort => Some(AppAction::Abort),
        AppEvent::Quit => Some(AppAction::Quit),
        AppEvent::Tick => Some(AppAction::Tick),
        AppEvent::CompactBoundary => Some(AppAction::CompactBoundary),
        AppEvent::Key(_) => None, // Already handled by the terminal branch
    }
}
