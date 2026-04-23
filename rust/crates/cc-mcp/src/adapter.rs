//! Adapter that exposes an MCP tool as a `cc_tools::Tool`,
//! so the query engine can call MCP tools through the same path as built-in tools.

use std::sync::Arc;

use async_trait::async_trait;
use cc_core::{CcResult, ToolInputSchema};
use cc_tools::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::transport::McpTransport;
use crate::{McpClient, McpHttpClient};

/// A single MCP tool surfaced as a `cc_tools::Tool`. Multiple adapters can share
/// the same `Arc<dyn McpTransport>` when they came from the same server.
pub struct McpToolAdapter {
    transport: Arc<dyn McpTransport>,
    /// Public name as the model sees it: `mcp__<server>__<tool>`.
    api_name: String,
    /// Original (unprefixed) tool name; what we send back to the MCP server.
    inner_name: String,
    description: String,
    input_schema: Value,
}

impl McpToolAdapter {
    pub fn new(
        transport: Arc<dyn McpTransport>,
        api_name: impl Into<String>,
        inner_name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
    ) -> Self {
        McpToolAdapter {
            transport,
            api_name: api_name.into(),
            inner_name: inner_name.into(),
            description: description.into(),
            input_schema,
        }
    }
}

#[async_trait]
impl Tool for McpToolAdapter {
    fn name(&self) -> &str {
        &self.api_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> ToolInputSchema {
        serde_json::from_value(self.input_schema.clone()).unwrap_or(ToolInputSchema {
            kind: "object".into(),
            properties: None,
            required: None,
            additional_properties: None,
        })
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> CcResult<ToolResult> {
        let result = self
            .transport
            .request(
                "tools/call",
                Some(json!({ "name": self.inner_name, "arguments": input })),
                &ctx.cancel,
            )
            .await;

        match result {
            Ok(value) => {
                let is_error = value
                    .get("isError")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                let content = if let Some(arr) = value.get("content").and_then(|v| v.as_array()) {
                    arr.iter()
                        .filter_map(|block| {
                            if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                                block
                                    .get("text")
                                    .and_then(|t| t.as_str())
                                    .map(|s| s.to_string())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                } else {
                    value.to_string()
                };

                if is_error {
                    Ok(ToolResult::error(content))
                } else {
                    Ok(ToolResult::ok(content))
                }
            }
            Err(e) => Ok(ToolResult::error(format!("MCP call failed: {e}"))),
        }
    }
}

/// Helper used by `main.rs` (and tests) to convert raw `mcpServers` JSON from
/// settings.json into a vector of connected `Tool` adapters.
pub async fn load_mcp_tools_from_config(mcp_servers: &Value) -> (Vec<Arc<dyn Tool>>, Vec<String>) {
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    let Some(map) = mcp_servers.as_object() else {
        return (tools, errors);
    };

    for (server_name, cfg) in map {
        let result = connect_one_server(server_name, cfg).await;
        match result {
            Ok(server_tools) => tools.extend(server_tools),
            Err(e) => errors.push(format!("MCP server '{server_name}': {e}")),
        }
    }

    (tools, errors)
}

async fn connect_one_server(server_name: &str, cfg: &Value) -> Result<Vec<Arc<dyn Tool>>, String> {
    let url = cfg.get("url").and_then(|v| v.as_str());
    let command = cfg.get("command").and_then(|v| v.as_str());

    let (transport, mcp_tools): (Arc<dyn McpTransport>, _) = match (url, command) {
        (Some(url), _) => {
            let mut client = McpHttpClient::connect(server_name, url).await?;
            let tools = client.list_tools().await?;
            (Arc::new(Mutex::new(client)), tools)
        }
        (None, Some(command)) => {
            let args: Vec<String> = cfg
                .get("args")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|a| a.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
            let mut client = McpClient::connect(server_name, command, &arg_refs).await?;
            let tools = client.list_tools().await?;
            (Arc::new(Mutex::new(client)), tools)
        }
        (None, None) => {
            return Err("server config has neither `url` nor `command`".to_string());
        }
    };

    let adapters: Vec<Arc<dyn Tool>> = mcp_tools
        .into_iter()
        .map(|t| {
            let api_name = format!("mcp__{}__{}", server_name, t.name);
            let description = t
                .description
                .clone()
                .unwrap_or_else(|| format!("MCP tool {} from {server_name}", t.name));
            Arc::new(McpToolAdapter::new(
                Arc::clone(&transport),
                api_name,
                t.name.clone(),
                description,
                t.input_schema.clone(),
            )) as Arc<dyn Tool>
        })
        .collect();

    Ok(adapters)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn empty_config_returns_no_tools() {
        let (tools, errors) = load_mcp_tools_from_config(&json!({})).await;
        assert!(tools.is_empty());
        assert!(errors.is_empty());
    }

    #[tokio::test]
    async fn invalid_server_records_error_without_panicking() {
        let cfg = json!({
            "broken": { "url": "http://127.0.0.1:1" }
        });
        let (tools, errors) = load_mcp_tools_from_config(&cfg).await;
        assert!(tools.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("broken"));
    }

    #[tokio::test]
    async fn missing_url_and_command_is_an_error() {
        let cfg = json!({
            "weird": { "note": "no transport here" }
        });
        let (tools, errors) = load_mcp_tools_from_config(&cfg).await;
        assert!(tools.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("neither"));
    }
}
