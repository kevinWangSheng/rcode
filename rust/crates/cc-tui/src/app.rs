//! TUI application state — extracted from rendering and async runtime so it can
//! be unit-tested headlessly.

use std::collections::VecDeque;

/// One transcript entry. Tool calls and assistant text are flattened into a flat
/// list so we can render them in order without recovering structure from the
/// underlying `MessageParam` blocks. This is intentionally a UI-only model;
/// authoritative history lives in the session JSONL.
#[derive(Debug, Clone)]
pub enum TranscriptItem {
    User(String),
    Assistant(String),
    /// System / info messages — slash command output, errors, info banners.
    System(String),
    /// A compaction marker; rendered specially to satisfy the M3 exit criterion
    /// "after auto-compact, TUI shows compact boundary in transcript".
    CompactBoundary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamState {
    Idle,
    Streaming,
    /// Engine is running tool calls between assistant turns.
    ToolUse,
}

/// State of the permission modal — `Some` means a dialog is currently shown.
#[derive(Debug, Clone)]
pub struct PendingPermission {
    pub tool_name: String,
    pub summary: String,
}

/// Top-level application state.
pub struct App {
    /// Transcript shown in the main panel, oldest first.
    pub transcript: Vec<TranscriptItem>,
    /// Currently-streaming assistant text — committed to `transcript` on stream end.
    pub streaming_text: String,
    /// User input buffer (the bottom box).
    pub input: String,
    /// Stream FSM.
    pub stream_state: StreamState,
    /// One-line status text below the input box.
    pub status: String,
    /// Set when the user requests quit; the main loop honors this on the next tick.
    pub should_quit: bool,
    /// Active permission dialog, if any.
    pub permission: Option<PendingPermission>,
    /// Bounded scroll offset from the bottom of the transcript.
    pub scroll: u16,
    /// Pending input lines submitted while a stream was running.
    /// Drained when the current stream finishes.
    pub queued: VecDeque<String>,
    /// Session ID, displayed in the title bar.
    pub session_id: String,
}

impl App {
    pub fn new(session_id: String) -> Self {
        Self {
            transcript: Vec::new(),
            streaming_text: String::new(),
            input: String::new(),
            stream_state: StreamState::Idle,
            status: String::from("Type a message. /help for commands. Ctrl+Q to quit."),
            should_quit: false,
            permission: None,
            scroll: 0,
            queued: VecDeque::new(),
            session_id,
        }
    }

    pub fn push_user(&mut self, text: String) {
        self.transcript.push(TranscriptItem::User(text));
        self.scroll = 0;
    }

    pub fn push_system(&mut self, text: String) {
        self.transcript.push(TranscriptItem::System(text));
        self.scroll = 0;
    }

    pub fn push_compact_boundary(&mut self) {
        self.transcript.push(TranscriptItem::CompactBoundary);
        self.scroll = 0;
    }

    pub fn start_stream(&mut self) {
        self.stream_state = StreamState::Streaming;
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
            self.transcript.push(TranscriptItem::Assistant(text));
        }
        self.stream_state = StreamState::Idle;
    }

    /// Mark the stream as aborted, preserving any partial text already received.
    pub fn abort_stream(&mut self) {
        if !self.streaming_text.is_empty() {
            let mut text = std::mem::take(&mut self.streaming_text);
            text.push_str(" [aborted]");
            self.transcript.push(TranscriptItem::Assistant(text));
        }
        self.stream_state = StreamState::Idle;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_lifecycle_commits_text() {
        let mut app = App::new("sess".into());
        app.push_user("hi".into());
        app.start_stream();
        app.on_token("hel");
        app.on_token("lo");
        assert_eq!(app.streaming_text, "hello");
        app.finish_stream();
        assert!(app.streaming_text.is_empty());
        assert!(matches!(
            app.transcript.last(),
            Some(TranscriptItem::Assistant(t)) if t == "hello"
        ));
        assert_eq!(app.stream_state, StreamState::Idle);
    }

    #[test]
    fn abort_preserves_partial_text() {
        let mut app = App::new("s".into());
        app.start_stream();
        app.on_token("partial");
        app.abort_stream();
        match app.transcript.last() {
            Some(TranscriptItem::Assistant(t)) => assert!(t.contains("partial") && t.contains("aborted")),
            _ => panic!("expected assistant message with aborted marker"),
        }
    }

    #[test]
    fn compact_boundary_appears_in_transcript() {
        let mut app = App::new("s".into());
        app.push_compact_boundary();
        assert!(matches!(app.transcript.last(), Some(TranscriptItem::CompactBoundary)));
    }
}
