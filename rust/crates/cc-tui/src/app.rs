//! TUI application state — extracted from rendering and async runtime so it can
//! be unit-tested headlessly.

use std::collections::VecDeque;

use cc_core::PromptDecision;
use tokio::sync::oneshot;

/// One transcript entry. Tool calls and assistant text are flattened into a flat
/// list so we can render them in order without recovering structure from the
/// underlying `MessageParam` blocks. This is intentionally a UI-only model;
/// authoritative history lives in the session JSONL.
#[derive(Debug, Clone)]
pub enum TranscriptItem {
    UserMessage(String),
    AssistantText(String),
    ToolCall { name: String, input_summary: String },
    ToolResult { name: String, output: String, is_error: bool },
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
        self.transcript.push(TranscriptItem::ToolCall { name, input_summary });
        self.scroll = 0;
    }

    pub fn push_tool_result(&mut self, name: String, output: String, is_error: bool) {
        self.transcript.push(TranscriptItem::ToolResult { name, output, is_error });
        self.scroll = 0;
    }

    pub fn push_compact_boundary(&mut self) {
        self.transcript.push(TranscriptItem::CompactBoundary);
        self.scroll = 0;
    }

    pub fn start_stream(&mut self) {
        self.mode = AppMode::Streaming;
        self.streaming_text.clear();
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
    }

    /// Mark the stream as aborted, preserving any partial text already received.
    pub fn abort_stream(&mut self) {
        if !self.streaming_text.is_empty() {
            let mut text = std::mem::take(&mut self.streaming_text);
            text.push_str(" [aborted]");
            self.transcript.push(TranscriptItem::AssistantText(text));
        }
        self.mode = AppMode::Input;
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
            Some(TranscriptItem::AssistantText(t)) => assert!(t.contains("partial") && t.contains("aborted")),
            _ => panic!("expected assistant message with aborted marker"),
        }
    }

    #[test]
    fn compact_boundary_appears_in_transcript() {
        let mut app = App::new("s".into(), "m".into());
        app.push_compact_boundary();
        assert!(matches!(app.transcript.last(), Some(TranscriptItem::CompactBoundary)));
    }

    #[test]
    fn tool_call_and_result_in_transcript() {
        let mut app = App::new("s".into(), "m".into());
        app.push_tool_call("Bash".into(), "ls -la".into());
        app.push_tool_result("Bash".into(), "file1.rs\nfile2.rs".into(), false);
        assert!(matches!(&app.transcript[0], TranscriptItem::ToolCall { name, .. } if name == "Bash"));
        assert!(matches!(&app.transcript[1], TranscriptItem::ToolResult { is_error, .. } if !is_error));
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
