pub mod bash;
pub mod edit;
pub mod glob_tool;
pub mod grep;
pub mod read;
pub mod write;

use async_trait::async_trait;
use cc_core::{CcResult, ToolDefinition, ToolInputSchema};
use serde_json::Value;
use std::sync::Arc;

/// Result of executing a tool call.
#[derive(Debug, Clone)]
pub struct ToolResult {
    /// Text content returned to the model.
    pub content: String,
    /// If true, the tool call is considered an error (permission denied, etc.).
    /// Bash failures use `is_error: false` — the exit code is included in `content`.
    pub is_error: bool,
}

impl ToolResult {
    pub fn ok(content: impl Into<String>) -> Self {
        ToolResult {
            content: content.into(),
            is_error: false,
        }
    }

    pub fn error(content: impl Into<String>) -> Self {
        ToolResult {
            content: content.into(),
            is_error: true,
        }
    }
}

/// Trait implemented by every built-in tool.
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    /// JSON Schema object for input parameters.
    fn input_schema(&self) -> Value;
    /// Whether this tool only reads state (allows concurrent execution).
    fn is_read_only(&self) -> bool {
        false
    }
    async fn execute(&self, input: Value) -> CcResult<ToolResult>;
}

/// Convert a `&dyn Tool` into a `ToolDefinition` for the API request.
pub fn tool_definition(tool: &dyn Tool) -> ToolDefinition {
    let schema_val = tool.input_schema();
    let schema: ToolInputSchema = serde_json::from_value(schema_val).unwrap_or(ToolInputSchema {
        kind: "object".into(),
        properties: None,
        required: None,
    });
    ToolDefinition {
        name: tool.name().to_string(),
        description: tool.description().to_string(),
        input_schema: schema,
        cache_control: None,
    }
}

/// Build the default set of built-in tools.
pub fn default_tools() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(bash::BashTool),
        Arc::new(read::ReadTool),
        Arc::new(write::WriteTool),
        Arc::new(edit::EditTool),
        Arc::new(glob_tool::GlobTool),
        Arc::new(grep::GrepTool),
    ]
}
