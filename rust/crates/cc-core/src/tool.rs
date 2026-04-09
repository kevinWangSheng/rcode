use crate::error::CcResult;
use crate::message::CacheControl;
use serde::{Deserialize, Serialize};
use serde_json::Value;
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
    /// `cancel` is checked periodically — tools should return early on cancellation.
    async fn execute(&self, input: Value, cancel: &CancellationToken) -> CcResult<ToolResult>;

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
}
