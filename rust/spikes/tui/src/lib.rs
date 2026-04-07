//! Core App logic — extracted for headless testing (no Ratatui dependency).

use std::time::Instant;

#[derive(Debug, Clone, PartialEq)]
pub enum StreamState {
    Idle,
    Streaming,
    Aborted,
}

pub struct App {
    pub history: Vec<(String, String)>,
    pub streaming_text: String,
    pub input: String,
    pub queued_inputs: Vec<String>,
    pub stream_state: StreamState,
    pub token_count: u64,
    pub turn_count: u64,
    pub abort_requested_at: Option<Instant>,
    pub last_abort_latency_ms: Option<u128>,
    pub status: String,
    pub should_quit: bool,
    pub session_start: Instant,
}

impl App {
    pub fn new() -> Self {
        Self {
            history: Vec::new(),
            streaming_text: String::new(),
            input: String::new(),
            queued_inputs: Vec::new(),
            stream_state: StreamState::Idle,
            token_count: 0,
            turn_count: 0,
            abort_requested_at: None,
            last_abort_latency_ms: None,
            status: String::new(),
            should_quit: false,
            session_start: Instant::now(),
        }
    }

    pub fn start_stream(&mut self) {
        self.stream_state = StreamState::Streaming;
        self.token_count = 0;
    }

    /// Called for each token arriving from the stream.
    /// Drops tokens silently if already aborted.
    pub fn on_token(&mut self, token: String) {
        if self.stream_state == StreamState::Aborted {
            if let Some(t) = self.abort_requested_at.take() {
                self.last_abort_latency_ms = Some(t.elapsed().as_millis());
            }
            return;
        }
        self.streaming_text.push_str(&token);
        self.token_count += 1;
    }

    /// Called when the stream finishes (naturally or after abort).
    pub fn on_stream_done(&mut self) {
        if self.stream_state == StreamState::Aborted {
            if let Some(t) = self.abort_requested_at.take() {
                self.last_abort_latency_ms = Some(t.elapsed().as_millis());
            }
            self.history.push((
                format!("turn {}", self.turn_count + 1),
                format!("{} [ABORTED]", self.streaming_text),
            ));
        } else {
            self.history.push((
                format!("turn {}", self.turn_count + 1),
                self.streaming_text.clone(),
            ));
        }
        self.streaming_text.clear();
        self.turn_count += 1;
        self.stream_state = StreamState::Idle;
        self.abort_requested_at = None;
    }

    /// Called when the user presses Ctrl+C.
    pub fn on_abort(&mut self) {
        if self.stream_state == StreamState::Streaming {
            self.abort_requested_at = Some(Instant::now());
            self.stream_state = StreamState::Aborted;
        }
    }

    /// Called when the user presses Enter.
    pub fn on_submit(&mut self, text: String) {
        if text.trim().is_empty() {
            return;
        }
        if self.stream_state == StreamState::Streaming
            || self.stream_state == StreamState::Aborted
        {
            // [AC-3] Queue during active stream
            self.queued_inputs.push(text);
        }
        // (if Idle, caller is responsible for starting the stream)
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}
