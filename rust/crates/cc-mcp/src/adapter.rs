//! Adapter that exposes an MCP tool (over stdio or HTTP) as a `cc_tools::Tool`,
//! so the query engine can call MCP tools through the same path as built-in tools.
//!
//! Each adapter holds a shared handle to a transport (stdio child or HTTP client)
//! plus the tool's metadata. Calls are dispatched through `Mutex` because the
//! underlying transports are not internally cloneable.

use std::sync::Arc;

use async_trait::async_trait;
use cc_core::{CcResult, ToolInputSchema};
use cc_tools::{Tool, ToolResult};
use serde_json::Value;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::{McpClient, McpHttpClient};

/// One of the supported MCP transports. The Stdio variant carries a child
/// process handle (large), so it is boxed to keep the enum small per
/// `clippy::large_enum_variant`. The transport itself lives behind an
/// `Arc<Mutex<...>>` so multiple tool adapters from one server share the
/// same connection.
pub enum McpTransport {
    Stdio(Box<Mutex<McpClient>>),
    Http(Mutex<McpHttpClient>),
}

/// A single MCP tool surfaced as a `cc_tools::Tool`. Multiple adapters can share
/// the same `Arc<McpTransport>` when they came from the same server.
pub struct McpToolAdapter {
    transport: Arc<McpTransport>,
    /// Public name as the model sees it: `mcp__<server>__<tool>`.
    api_name: String,
    /// Original (unprefixed) tool name; what we send back to the MCP server.
    inner_name: String,
    description: String,
    input_schema: Value,
}

impl McpToolAdapter {
    pub fn new(
        transport: Arc<McpTransport>,
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

    async fn execute(&self, input: Value, _cancel: &CancellationToken) -> CcResult<ToolResult> {
        let result = match &*self.transport {
            McpTransport::Stdio(client) => {
                let mut guard = client.lock().await;
                guard.call_tool(&self.inner_name, input).await
            }
            McpTransport::Http(client) => {
                let mut guard = client.lock().await;
                guard.call_tool(&self.inner_name, input).await
            }
        };

        if result.is_error {
            Ok(ToolResult::error(result.content))
        } else {
            Ok(ToolResult::ok(result.content))
        }
    }
}

/// Helper used by `main.rs` (and tests) to convert raw `mcpServers` JSON from
/// settings.json into a vector of connected `Tool` adapters. Returns a tuple of
/// `(adapters, errors)` — failures connecting to any single server are reported
/// in `errors` and do not prevent the rest from loading.
///
/// `mcpServers` shape (matches the TS schema):
/// ```jsonc
/// {
///   "mcpServers": {
///     "filesystem": { "command": "npx", "args": ["@mcp/filesystem"] },
///     "remote":     { "url": "https://mcp.example.com/sse" }
///   }
/// }
/// ```
pub async fn load_mcp_tools_from_config(
    mcp_servers: &Value,
) -> (Vec<Arc<dyn Tool>>, Vec<String>) {
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
    // Decide transport: presence of `url` → HTTP, presence of `command` → stdio.
    let url = cfg.get("url").and_then(|v| v.as_str());
    let command = cfg.get("command").and_then(|v| v.as_str());

    let (transport, mcp_tools) = match (url, command) {
        (Some(url), _) => {
            let mut client = McpHttpClient::connect(server_name, url).await?;
            let tools = client.list_tools().await?;
            (Arc::new(McpTransport::Http(Mutex::new(client))), tools)
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
            (
                Arc::new(McpTransport::Stdio(Box::new(Mutex::new(client)))),
                tools,
            )
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
        // Bogus URL → HTTP connect should fail and surface as an error string.
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
