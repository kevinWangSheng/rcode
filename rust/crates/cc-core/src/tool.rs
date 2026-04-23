use crate::error::CcResult;
use crate::file_history::FileHistorySnapshot;
use crate::message::CacheControl;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Result of executing a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub content: String,
    pub is_error: bool,
}

impl ToolResult {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
        }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
        }
    }
}

/// JSON Schema for a tool's input parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInputSchema {
    #[serde(rename = "type")]
    pub kind: String, // "object"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub properties: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<Vec<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "additionalProperties"
    )]
    pub additional_properties: Option<bool>,
}

/// A tool definition sent to the Anthropic API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: ToolInputSchema,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

/// Session-side append hooks that tools invoke through `ToolContext::session`.
///
/// Kept as a narrow trait so `cc-core` can define the contract without
/// depending on `cc-session` (which already depends on `cc-core`). Extend
/// only when an actual caller needs a new method — every addition is a
/// breaking change for the trait's implementers.
pub trait SessionSink: Send + Sync {
    /// Append a canonical interrupt-marker user message. `for_tool_use`
    /// picks the tool-use variant string (matches TS
    /// `createUserInterruptionMessage`).
    fn append_interrupt_marker(&self, for_tool_use: bool) -> CcResult<()>;

    /// Append a file-history snapshot as a
    /// `{type: "file-history-snapshot"}` JSONL entry. Mirrors TS
    /// `sessionStorage.insertFileHistorySnapshot`.
    fn append_file_history_snapshot(
        &self,
        snapshot: &FileHistorySnapshot,
        is_update: bool,
    ) -> CcResult<()>;

    /// High-level helper used by `Edit` / `Write` to persist the pre-
    /// mutation bytes of a file they're about to rewrite. Writes the bytes
    /// into a per-session sidecar (matches TS
    /// `{configDir}/file-history/{sessionId}/{hash}@v{n}`) and appends a
    /// `FileHistorySnapshot` JSONL entry that references it by
    /// `backup_file_name`.
    ///
    /// `relpath` SHOULD be project-relative when possible; the TS path-
    /// shortening helper already lives in `cc_session` and is reused here.
    /// `message_id` MUST be the assistant-turn id that produced this
    /// tool_use; an empty string is permitted only for isolated tool
    /// tests where no message is in flight.
    ///
    /// Default impl returns `Ok(())` so implementers that don't manage an
    /// on-disk session (e.g. `NoopSessionSink`) silently succeed.
    fn append_file_history_snapshot_for_path(
        &self,
        _relpath: &str,
        _message_id: &str,
        _prior_bytes: &[u8],
        _is_update: bool,
    ) -> CcResult<()> {
        Ok(())
    }
}

/// Runtime dependencies a tool needs beyond its `input` JSON.
///
/// Passed by reference to every `Tool::execute` call. New fields may be
/// added additively; do not remove or rename existing fields without a
/// spec update — external-style consumers (MCP adapter, cc-agents drivers)
/// construct these as well.
#[derive(Clone)]
pub struct ToolContext {
    /// Handle to the caller's session, so tools can append
    /// side-effect entries (file-history snapshots, interrupt
    /// markers, etc.).
    pub session: Arc<dyn SessionSink>,

    /// Cooperative cancellation. Tools should poll
    /// `ctx.cancel.is_cancelled()` in long-running loops.
    pub cancel: CancellationToken,

    /// The assistant-turn message id that produced this tool_use.
    /// Currently populated from `cc-query` dispatch (best-effort);
    /// `None` in isolated tool tests.
    pub message_id: Option<String>,
}

impl ToolContext {
    /// Construct a bare ctx for tests / call sites that have no real
    /// session. The sink's append methods are no-ops (return `Ok(())`).
    ///
    /// Kept on the production path (not `#[cfg(test)]`) because
    /// workspace-internal crates such as `cc-agents` driver tasks need a
    /// real `ToolContext` when no caller session is available, and
    /// downstream users of the library also need a sanctioned way to
    /// build a minimal ctx.
    pub fn for_test_bare(cancel: CancellationToken) -> Self {
        ToolContext {
            session: Arc::new(NoopSessionSink),
            cancel,
            message_id: None,
        }
    }
}

/// Session sink whose append methods silently succeed. Intended for
/// tests and non-session contexts (driver tasks that run tools without a
/// caller session). Production code paths MUST wire a real `Session`
/// implementation.
struct NoopSessionSink;

impl SessionSink for NoopSessionSink {
    fn append_interrupt_marker(&self, _for_tool_use: bool) -> CcResult<()> {
        Ok(())
    }

    fn append_file_history_snapshot(
        &self,
        _snapshot: &FileHistorySnapshot,
        _is_update: bool,
    ) -> CcResult<()> {
        Ok(())
    }
}

/// The trait all tools (built-in + MCP) implement.
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    /// Unique tool name (e.g. "Bash", "mcp__fs__read_file").
    fn name(&self) -> &str;

    fn description(&self) -> &str;

    fn input_schema(&self) -> ToolInputSchema;

    /// Whether this tool only reads state (no side effects).
    /// Read-only tools may run concurrently; mutating tools are serialized.
    fn is_read_only(&self) -> bool {
        false
    }

    /// Execute the tool with the given JSON input.
    /// `ctx.cancel` is checked periodically — tools should return early on
    /// cancellation. `ctx.session` lets the tool append side-effect
    /// entries (file-history snapshots, interrupt markers).
    async fn execute(&self, input: Value, ctx: &ToolContext) -> CcResult<ToolResult>;

    /// Convert to API ToolDefinition.
    fn to_definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: self.description().to_string(),
            input_schema: self.input_schema(),
            cache_control: None,
        }
    }
}

/// Type-erased tool container used by the query engine.
pub type BoxTool = Box<dyn Tool>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_result_ok() {
        let r = ToolResult::ok("output");
        assert!(!r.is_error);
        assert_eq!(r.content, "output");
    }

    #[test]
    fn tool_result_error() {
        let r = ToolResult::error("failed");
        assert!(r.is_error);
        assert_eq!(r.content, "failed");
    }

    #[test]
    fn tool_input_schema_serialization() {
        let schema = ToolInputSchema {
            kind: "object".to_string(),
            properties: Some(serde_json::json!({"command": {"type": "string"}})),
            required: Some(vec!["command".to_string()]),
            additional_properties: Some(false),
        };
        let json = serde_json::to_value(&schema).unwrap();
        assert_eq!(json["type"], "object");
        assert_eq!(json["additionalProperties"], false);
    }

    #[test]
    fn for_test_bare_noop_sink_round_trips() {
        let cancel = CancellationToken::new();
        let ctx = ToolContext::for_test_bare(cancel);
        assert!(ctx.session.append_interrupt_marker(false).is_ok());
        assert!(ctx.session.append_interrupt_marker(true).is_ok());
        assert!(ctx.message_id.is_none());
    }
}
