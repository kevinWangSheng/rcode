use crate::message::{StopReason, ToolUseBlock, Usage};
use crate::permission::PromptDecision;
use crate::task::{TaskId, TaskNotification};
use crate::tool::ToolResult;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::oneshot;

/// Events sent from engine/hooks/tasks to the TUI.
#[derive(Debug)]
pub enum AppEvent {
    // Streaming
    StreamDelta(String),
    StreamThinking(String),
    StreamToolUse(ToolUseBlock),
    StreamEnd(StopReason),

    // Tool execution
    ToolStart { name: String, input: Value },
    ToolEnd { name: String, result: ToolResult },

    // Permission — engine sends request with a oneshot channel;
    // TUI renders dialog and sends decision back through `response_tx`.
    PermissionRequest {
        id: u64,
        tool_name: String,
        tool_input: Value,
        response_tx: oneshot::Sender<PromptDecision>,
    },

    // Session
    CompactBoundary,
    TurnComplete { usage: Usage },

    // Tasks
    TaskUpdate(TaskNotification),

    // Fatal
    Error(String),
}

/// Status bar state (broadcast to all TUI components).
#[derive(Debug, Clone, Default)]
pub struct StatusLine {
    pub model: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cost_usd: f64,
    pub active_tasks: usize,
    pub session_id: String,
}

/// Teammate → parent message kinds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeammateMessage {
    pub task_id: TaskId,
    pub kind: TeammateMessageKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TeammateMessageKind {
    StatusUpdate(String),
    Output(String),
    PermissionEscalation {
        tool_name: String,
        tool_input: Value,
    },
    Completed {
        summary: String,
    },
    Failed {
        error: String,
    },
}
