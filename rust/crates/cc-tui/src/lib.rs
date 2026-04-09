//! cc-tui — interactive terminal UI for `claude`.
//!
//! Absorbs cc-commands (slash command registry) per Phase 2 Decision 3.
//! Provides Ratatui-based TUI with streaming render, permission dialogs,
//! and slash command dispatch.

pub mod app;
pub mod commands;
pub mod event;
pub mod keybindings;
pub mod prompter;
pub mod render;

pub use app::{App, AppMode, StatusLine, TranscriptItem};
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
/// Full implementation with `tokio::select!` main loop will be wired
/// when the engine↔TUI channel protocol is finalized. For now this
/// sets up the terminal and renders the initial state.
pub async fn run_tui(config: TuiConfig) -> cc_core::CcResult<()> {
    let app = App::new(config.session_id, config.model);

    // Initialize terminal
    let mut terminal = ratatui::init();

    // Render initial frame
    terminal
        .draw(|frame| render::render(frame, &app))
        .map_err(|e| cc_core::CcError::Other(format!("terminal draw error: {e}")))?;

    // Restore terminal on exit
    ratatui::restore();

    // The full main loop (tokio::select over crossterm events, engine events,
    // and tick interval) will be wired in the integration layer when the
    // QueryEngine↔TUI channel protocol is finalized.
    Ok(())
}
