//! Internal event union driving the TUI main loop.
//!
//! Events come from three places:
//!   1. The blocking input thread reading crossterm events.
//!   2. The engine task as it streams tokens / asks for permission.
//!   3. Internal scheduling (e.g. tick, redraw).

use crossterm::event::KeyEvent;
use serde_json::Value;
use tokio::sync::oneshot;

use cc_query::PromptDecision;

/// Events delivered to the main loop on a single mpsc channel.
#[derive(Debug)]
pub enum AppEvent {
    /// A raw key event from the terminal.
    Key(KeyEvent),
    /// One streaming text delta from the engine.
    Token(String),
    /// The current engine turn finished naturally — `Ok` carries the final assistant text.
    EngineDone(Result<String, String>),
    /// The engine asked for permission. The TUI must respond on `reply`.
    PermissionRequest {
        tool_name: String,
        input: Value,
        reply: oneshot::Sender<PromptDecision>,
    },
    /// User-triggered abort (Ctrl+C).
    Abort,
    /// User asked to quit (Ctrl+Q).
    Quit,
    /// Periodic tick — used to refresh time-sensitive UI elements.
    Tick,
}
