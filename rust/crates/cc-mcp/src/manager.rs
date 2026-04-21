//! MCP server lifecycle management (§9.2).

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::Mutex;
use tracing::{debug, warn};

use cc_core::CcResult;
use cc_tools::Tool;

use crate::adapter::McpToolAdapter;
use crate::client::McpClient;
use crate::http_client::McpHttpClient;
use crate::transport::McpTransport;
use crate::types::McpTool;

/// MCP server connection state.
#[derive(Debug)]
pub enum McpServerState {
    Pending,
    Connected(ConnectedServer),
    Failed { error: String, config: Value },
    NeedsAuth { auth_url: String },
    Disabled,
}

/// A connected MCP server.
pub struct ConnectedServer {
    pub name: String,
    pub transport: Arc<dyn McpTransport>,
    pub tools: Vec<McpTool>,
}

impl std::fmt::Debug for ConnectedServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectedServer")
            .field("name", &self.name)
            .field("tools", &self.tools.len())
            .finish()
    }
}

/// MCP server manager — handles all configured servers.
pub struct McpManager {
    servers: HashMap<String, McpServerState>,
}

impl McpManager {
    /// Initialize all servers from config.
    ///
    /// No `reqwest::Client` parameter: the manager only speaks to MCP servers
    /// through `McpHttpClient` (HTTP Streamable transport), which builds its
    /// own client, and `McpClient` (stdio transport), which doesn't need
    /// HTTP at all. A previously threaded `http: reqwest::Client` field was
    /// never read anywhere and has been removed.
    pub async fn init_from_config(config: &HashMap<String, Value>) -> Self {
        let mut manager = McpManager {
            servers: HashMap::new(),
        };

        for (name, cfg) in config {
            manager
                .servers
                .insert(name.clone(), McpServerState::Pending);
            if let Err(e) = manager.connect_server(name, cfg).await {
                warn!("MCP server '{name}' failed to connect: {e}");
                manager.servers.insert(
                    name.clone(),
                    McpServerState::Failed {
                        error: e.to_string(),
                        config: cfg.clone(),
                    },
                );
            }
        }

        manager
    }

    /// Connect a single server.
    async fn connect_server(&mut self, name: &str, config: &Value) -> CcResult<()> {
        let url = config.get("url").and_then(|v| v.as_str());
        let command = config.get("command").and_then(|v| v.as_str());

        let (transport, tools): (Arc<dyn McpTransport>, Vec<McpTool>) = match (url, command) {
            (Some(url), _) => {
                // Pull optional headers out of the config — used for things
                // like `Authorization: Bearer <token>` on OAuth-protected
                // MCP servers. Non-string values are silently skipped.
                let headers: HashMap<String, String> = config
                    .get("headers")
                    .and_then(|v| v.as_object())
                    .map(|m| {
                        m.iter()
                            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                            .collect()
                    })
                    .unwrap_or_default();

                // Optional `timeout_ms` per-server override. Accepts integer
                // or string-integer (settings.json is JSON, but we lean
                // lenient on config parsing in case someone hand-edits).
                let timeout_ms = config
                    .get("timeout_ms")
                    .and_then(|v| {
                        v.as_u64()
                            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                    })
                    .unwrap_or(30_000);

                let mut client = McpHttpClient::connect_with(name, url, headers, timeout_ms)
                    .await
                    .map_err(cc_core::CcError::Other)?;
                let tools = client.list_tools().await.map_err(cc_core::CcError::Other)?;
                (Arc::new(Mutex::new(client)), tools)
            }
            (None, Some(command)) => {
                let args: Vec<String> = config
                    .get("args")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|a| a.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
                let mut client = McpClient::connect(name, command, &arg_refs)
                    .await
                    .map_err(cc_core::CcError::Other)?;
                let tools = client.list_tools().await.map_err(cc_core::CcError::Other)?;
                (Arc::new(Mutex::new(client)), tools)
            }
            (None, None) => {
                return Err(cc_core::CcError::Other(format!(
                    "server '{name}' has neither `url` nor `command`"
                )));
            }
        };

        debug!("MCP server '{name}' connected with {} tools", tools.len());

        self.servers.insert(
            name.to_string(),
            McpServerState::Connected(ConnectedServer {
                name: name.to_string(),
                transport,
                tools,
            }),
        );

        Ok(())
    }

    /// Get all tools from all connected servers.
    /// Tools are prefixed: `mcp__<server>__<tool>`.
    pub fn all_tools(&self) -> Vec<Arc<dyn Tool>> {
        let mut result: Vec<Arc<dyn Tool>> = Vec::new();

        for (server_name, state) in &self.servers {
            if let McpServerState::Connected(server) = state {
                for tool in &server.tools {
                    let api_name = format!("mcp__{}__{}", server_name, tool.name);
                    let description = tool
                        .description
                        .clone()
                        .unwrap_or_else(|| format!("MCP tool {} from {server_name}", tool.name));
                    result.push(Arc::new(McpToolAdapter::new(
                        Arc::clone(&server.transport),
                        api_name,
                        tool.name.clone(),
                        description,
                        tool.input_schema.clone(),
                    )));
                }
            }
        }

        result
    }

    /// Reconnect a failed server.
    pub async fn reconnect(&mut self, name: &str) -> CcResult<()> {
        let config = match self.servers.get(name) {
            Some(McpServerState::Failed { config, .. }) => config.clone(),
            _ => {
                return Err(cc_core::CcError::Other(format!(
                    "server '{name}' is not in failed state"
                )));
            }
        };
        self.connect_server(name, &config).await
    }

    /// Gracefully close all servers.
    pub async fn shutdown(&mut self) {
        for (name, state) in self.servers.drain() {
            if let McpServerState::Connected(server) = state {
                if let Err(e) = server.transport.close().await {
                    debug!("error closing MCP server '{name}': {e}");
                }
            }
        }
    }

    /// Get the state of a specific server.
    pub fn server_state(&self, name: &str) -> Option<&McpServerState> {
        self.servers.get(name)
    }

    /// List all server names.
    pub fn server_names(&self) -> Vec<String> {
        self.servers.keys().cloned().collect()
    }
}

impl std::fmt::Debug for McpManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpManager")
            .field("servers", &self.servers.keys().collect::<Vec<_>>())
            .finish()
    }
}
