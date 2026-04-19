//! TUI application state — extracted from rendering and async runtime so it can
//! be unit-tested headlessly.

use std::collections::VecDeque;
use std::time::Instant;

use cc_core::PromptDecision;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::keybindings::Keybindings;

/// Window (in milliseconds) during which a second Ctrl+C escalates from
/// graceful `Abort` to `ForceQuit`.
pub const FORCE_QUIT_WINDOW_MS: u64 = 2_000;

/// One transcript entry. Tool calls and assistant text are flattened into a flat
/// list so we can render them in order without recovering structure from the
/// underlying `MessageParam` blocks. This is intentionally a UI-only model;
/// authoritative history lives in the session JSONL.
#[derive(Debug, Clone)]
pub enum TranscriptItem {
    UserMessage(String),
    AssistantText(String),
    ToolCall {
        name: String,
        input_summary: String,
        /// Raw JSON input. Kept so the renderer can special-case Edit
        /// (unified diff from `old_string` / `new_string`) and future
        /// tool-specific cards without re-plumbing the engine.
        raw_input: serde_json::Value,
    },
    ToolResult {
        name: String,
        output: String,
        is_error: bool,
    },
    /// A compaction marker; rendered specially.
    CompactBoundary,
    /// System / info messages — slash command output, errors, info banners.
    SystemNotice(String),
}

/// Application mode — what the TUI is currently doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMode {
    /// User typing input.
    Input,
    /// Model streaming a response.
    Streaming,
    /// Waiting for user to approve/deny a permission prompt.
    PermissionPrompt,
    /// Slash command autocomplete palette open.
    CommandPalette,
}

/// State of the permission modal — `Some` means a dialog is currently shown.
#[derive(Debug, Clone)]
pub struct PendingPermission {
    pub tool_name: String,
    pub summary: String,
}

/// Status bar data.
#[derive(Debug, Clone)]
pub struct StatusLine {
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub estimated_cost_usd: f64,
    pub turn_count: u32,
}

impl StatusLine {
    pub fn new(model: String) -> Self {
        Self {
            model,
            input_tokens: 0,
            output_tokens: 0,
            estimated_cost_usd: 0.0,
            turn_count: 0,
        }
    }

    pub fn format(&self) -> String {
        format!(
            "model: {} | tokens: {}/{} | ${:.4} | turns: {}",
            self.model,
            format_tokens(self.input_tokens),
            format_tokens(self.output_tokens),
            self.estimated_cost_usd,
            self.turn_count,
        )
    }
}

fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Top-level application state.
pub struct App {
    /// Application mode.
    pub mode: AppMode,
    /// Transcript shown in the main panel, oldest first.
    pub transcript: Vec<TranscriptItem>,
    /// Currently-streaming assistant text — committed to `transcript` on stream end.
    pub streaming_text: String,
    /// User input buffer (the bottom box).
    pub input: String,
    /// Status bar.
    pub status: StatusLine,
    /// Set when the user requests quit; the main loop honors this on the next tick.
    pub should_quit: bool,
    /// Active permission dialog, if any.
    pub permission: Option<PendingPermission>,
    /// Bounded scroll offset from the bottom of the transcript.
    pub scroll: u16,
    /// Pending input lines submitted while a stream was running.
    pub queued: VecDeque<String>,
    /// Session ID, displayed in the title bar.
    pub session_id: String,
    /// Reply channel for the current permission prompt.
    pub pending_reply: Option<oneshot::Sender<PromptDecision>>,
    /// Cancellation token for the currently-running engine turn (if any).
    /// Set on Submit; cancelled on Abort; cleared on TurnComplete.
    pub current_turn_cancel: Option<CancellationToken>,
    /// Timestamp of the most recent Ctrl+C (graceful abort) request.
    /// A second Ctrl+C within `FORCE_QUIT_WINDOW_MS` escalates to `ForceQuit`
    /// so the user always has an escape hatch even when the first abort is
    /// stuck (e.g. engine in a slow syscall).
    pub last_abort_at: Option<Instant>,
    /// Transient status-line hint (e.g. "press Ctrl+C again to force quit").
    /// Cleared by the render layer once the force-quit window has lapsed.
    pub status_hint: Option<String>,
    /// Snapshot of `mode` taken when a `ShowPermission` action fires. A
    /// permission dialog can appear both during `Streaming` (tool call mid-
    /// turn) and after it (final tool call arriving as the assistant block
    /// lands, mode already `Input`). Restoring from this snapshot on any
    /// decision keeps the TUI's input routing correct.
    pub pre_permission_mode: Option<AppMode>,
    /// Active keybinding map. Loaded once at startup and swapped in-place by
    /// `/reload-keybindings` (see `AppAction::ReloadKeybindings`). Held on the
    /// App so `map_key_event` and the reload action share a single source of
    /// truth without threading an `Arc<Mutex>` through the main loop.
    pub keybindings: Keybindings,
    /// When the current stream started — used by the status bar to render
    /// `elapsed` + by `spinner_glyph` for rotation. `None` means "no active
    /// stream" and the spinner must be hidden (AC-V2 second clause: cleared
    /// within 100 ms of turn end).
    pub stream_started_at: Option<Instant>,
    /// Animation frame, advanced once per `AppAction::Tick` while streaming.
    /// Ratatui redraws on every action so the spinner glyph refreshes at the
    /// tick cadence (100 ms) without any external timer.
    pub spinner_frame: u64,
    /// Cached command-palette matches for the current input filter. Computed
    /// each time the filter changes; rendered by `render_command_palette`.
    pub palette_matches: Vec<String>,
    /// Highlighted row index inside `palette_matches`. Clamped into range by
    /// the arrow-key handler.
    pub palette_selected: usize,
    /// Snapshot of `input` captured when the palette opened — allows Esc to
    /// restore the buffer byte-for-byte without exposing the partial `/`
    /// filter to a reader.
    pub palette_original: Option<String>,
}

impl App {
    pub fn new(session_id: String, model: String) -> Self {
        Self {
            mode: AppMode::Input,
            transcript: Vec::new(),
            streaming_text: String::new(),
            input: String::new(),
            status: StatusLine::new(model),
            should_quit: false,
            permission: None,
            scroll: 0,
            queued: VecDeque::new(),
            session_id,
            pending_reply: None,
            current_turn_cancel: None,
            last_abort_at: None,
            status_hint: None,
            pre_permission_mode: None,
            keybindings: Keybindings::default(),
            stream_started_at: None,
            spinner_frame: 0,
            palette_matches: Vec::new(),
            palette_selected: 0,
            palette_original: None,
        }
    }

    /// Current spinner glyph. Returns an empty string when no stream is
    /// active so the status bar is clean between turns.
    pub fn spinner_glyph(&self) -> &'static str {
        const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        if self.stream_started_at.is_none() {
            return "";
        }
        FRAMES[(self.spinner_frame as usize) % FRAMES.len()]
    }

    /// Number of user messages queued while a stream is running.
    pub fn queued_count(&self) -> usize {
        self.queued.len()
    }

    /// Reload the keybinding map via the provided loader.
    ///
    /// Production callers pass `Keybindings::try_load` so we re-read
    /// `~/.claude/keybindings.json`. Tests can pass a closure pointing at a
    /// tempdir fixture to avoid touching the real HOME.
    ///
    /// Return value:
    ///   - `Ok(count)` — new map applied (or file absent → defaults kept).
    ///     `count` is the number of first-class bindings active.
    ///   - `Err(msg)` — loader reported an error; `self.keybindings` is left
    ///     untouched so the previous map remains live (matches the "malformed
    ///     file during edit does not wipe cache" scenario).
    pub fn reload_keybindings_with<F>(&mut self, loader: F) -> Result<usize, String>
    where
        F: FnOnce() -> Result<Option<Keybindings>, String>,
    {
        match loader() {
            Ok(Some(kb)) => {
                self.keybindings = kb;
                // Three first-class actions today (Quit / Abort / Submit).
                Ok(3)
            }
            Ok(None) => {
                // File absent — treat as "no customisations". Keep whatever
                // is currently live (typically the default map).
                Ok(3)
            }
            Err(e) => Err(e),
        }
    }

    /// Has the user pressed Ctrl+C recently enough that a second press
    /// should force-quit rather than start a new graceful abort?
    pub fn within_force_quit_window(&self, now: Instant) -> bool {
        match self.last_abort_at {
            Some(stamp) => now.duration_since(stamp).as_millis() < FORCE_QUIT_WINDOW_MS as u128,
            None => false,
        }
    }

    pub fn push_user(&mut self, text: String) {
        self.transcript.push(TranscriptItem::UserMessage(text));
        self.scroll = 0;
    }

    pub fn push_system(&mut self, text: String) {
        self.transcript.push(TranscriptItem::SystemNotice(text));
        self.scroll = 0;
    }

    pub fn push_tool_call(&mut self, name: String, input_summary: String) {
        self.push_tool_call_with_input(name, input_summary, serde_json::Value::Null);
    }

    pub fn push_tool_call_with_input(
        &mut self,
        name: String,
        input_summary: String,
        raw_input: serde_json::Value,
    ) {
        self.transcript.push(TranscriptItem::ToolCall {
            name,
            input_summary,
            raw_input,
        });
        self.scroll = 0;
    }

    pub fn push_tool_result(&mut self, name: String, output: String, is_error: bool) {
        self.transcript.push(TranscriptItem::ToolResult {
            name,
            output,
            is_error,
        });
        self.scroll = 0;
    }

    pub fn push_compact_boundary(&mut self) {
        self.transcript.push(TranscriptItem::CompactBoundary);
        self.scroll = 0;
    }

    pub fn start_stream(&mut self) {
        self.mode = AppMode::Streaming;
        self.streaming_text.clear();
        self.stream_started_at = Some(Instant::now());
        self.spinner_frame = 0;
    }

    pub fn on_token(&mut self, delta: &str) {
        self.streaming_text.push_str(delta);
        self.scroll = 0;
    }

    /// Commit the in-progress streaming text to the transcript and reset state.
    pub fn finish_stream(&mut self) {
        if !self.streaming_text.is_empty() {
            let text = std::mem::take(&mut self.streaming_text);
            self.transcript.push(TranscriptItem::AssistantText(text));
        }
        self.mode = AppMode::Input;
        self.stream_started_at = None;
    }

    /// Mark the stream as aborted, preserving any partial text already received.
    pub fn abort_stream(&mut self) {
        if !self.streaming_text.is_empty() {
            let mut text = std::mem::take(&mut self.streaming_text);
            text.push_str(" [aborted]");
            self.transcript.push(TranscriptItem::AssistantText(text));
        }
        self.mode = AppMode::Input;
        self.stream_started_at = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_lifecycle_commits_text() {
        let mut app = App::new("sess".into(), "test-model".into());
        app.push_user("hi".into());
        app.start_stream();
        app.on_token("hel");
        app.on_token("lo");
        assert_eq!(app.streaming_text, "hello");
        app.finish_stream();
        assert!(app.streaming_text.is_empty());
        assert!(matches!(
            app.transcript.last(),
            Some(TranscriptItem::AssistantText(t)) if t == "hello"
        ));
        assert_eq!(app.mode, AppMode::Input);
    }

    #[test]
    fn abort_preserves_partial_text() {
        let mut app = App::new("s".into(), "m".into());
        app.start_stream();
        app.on_token("partial");
        app.abort_stream();
        match app.transcript.last() {
            Some(TranscriptItem::AssistantText(t)) => {
                assert!(t.contains("partial") && t.contains("aborted"))
            }
            _ => panic!("expected assistant message with aborted marker"),
        }
    }

    #[test]
    fn compact_boundary_appears_in_transcript() {
        let mut app = App::new("s".into(), "m".into());
        app.push_compact_boundary();
        assert!(matches!(
            app.transcript.last(),
            Some(TranscriptItem::CompactBoundary)
        ));
    }

    #[test]
    fn tool_call_and_result_in_transcript() {
        let mut app = App::new("s".into(), "m".into());
        app.push_tool_call("Bash".into(), "ls -la".into());
        app.push_tool_result("Bash".into(), "file1.rs\nfile2.rs".into(), false);
        assert!(
            matches!(&app.transcript[0], TranscriptItem::ToolCall { name, .. } if name == "Bash")
        );
        assert!(
            matches!(&app.transcript[1], TranscriptItem::ToolResult { is_error, .. } if !is_error)
        );
    }

    #[test]
    fn status_line_formatting() {
        let mut status = StatusLine::new("claude-sonnet-4-6".into());
        status.input_tokens = 1234;
        status.output_tokens = 567;
        let text = status.format();
        assert!(text.contains("claude-sonnet-4-6"));
        assert!(text.contains("1.2k"));
        assert!(text.contains("567"));
    }

    #[test]
    fn mode_transitions() {
        let mut app = App::new("s".into(), "m".into());
        assert_eq!(app.mode, AppMode::Input);
        app.start_stream();
        assert_eq!(app.mode, AppMode::Streaming);
        app.finish_stream();
        assert_eq!(app.mode, AppMode::Input);
    }
}
