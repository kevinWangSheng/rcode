use crate::transport::McpTransport;
use crate::types::{JsonRpcRequest, JsonRpcResponse, McpTool, McpToolResult};
use async_trait::async_trait;
use cc_core::{CcError, CcResult};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::debug;

static REQUEST_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    REQUEST_ID.fetch_add(1, Ordering::SeqCst)
}

/// MCP stdio client — connects to an MCP server spawned as a child process.
pub struct McpClient {
    pub server_name: String,
    stdin: Mutex<ChildStdin>,
    stdout: Mutex<BufReader<ChildStdout>>,
    _child: Child,
}

impl McpClient {
    /// Spawn an MCP server process and perform the `initialize` handshake.
    pub async fn connect(
        server_name: impl Into<String>,
        command: &str,
        args: &[&str],
    ) -> Result<Self, String> {
        use tokio::process::Command;
        let mut child = Command::new(command)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("failed to spawn MCP server '{command}': {e}"))?;

        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;

        let mut client = McpClient {
            server_name: server_name.into(),
            stdin: Mutex::new(stdin),
            stdout: Mutex::new(BufReader::new(stdout)),
            _child: child,
        };

        // Send initialize request
        let init_params = json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {
                "name": "claude-code",
                "version": "0.1.0"
            }
        });
        let resp = client
            .send_request("initialize", Some(init_params))
            .await
            .map_err(|e| format!("MCP initialize failed: {e}"))?;

        if let Some(err) = resp.error {
            return Err(format!("MCP initialize error: {} ({})", err.message, err.code));
        }

        debug!("MCP server '{}' initialized", client.server_name);

        // Send initialized notification
        client
            .send_notification("notifications/initialized", None)
            .await
            .map_err(|e| format!("MCP initialized notification failed: {e}"))?;

        Ok(client)
    }

    /// List tools available on this server.
    pub async fn list_tools(&mut self) -> Result<Vec<McpTool>, String> {
        let resp = self
            .send_request("tools/list", None)
            .await
            .map_err(|e| format!("tools/list failed: {e}"))?;

        if let Some(err) = resp.error {
            return Err(format!("tools/list error: {}", err.message));
        }

        let result = resp.result.unwrap_or(json!({}));
        let tools_val = result.get("tools").cloned().unwrap_or(json!([]));
        let mut tools: Vec<McpTool> = serde_json::from_value(tools_val)
            .map_err(|e| format!("failed to parse tools: {e}"))?;

        // Tag with server name
        for tool in &mut tools {
            tool.server_name = self.server_name.clone();
        }

        Ok(tools)
    }

    /// Call a tool on this server.
    pub async fn call_tool(&mut self, tool_name: &str, arguments: Value) -> McpToolResult {
        let params = json!({
            "name": tool_name,
            "arguments": arguments
        });

        let resp = match self.send_request("tools/call", Some(params)).await {
            Ok(r) => r,
            Err(e) => {
                return McpToolResult {
                    content: format!("MCP request error: {e}"),
                    is_error: true,
                };
            }
        };

        if let Some(err) = resp.error {
            return McpToolResult {
                content: format!("MCP error {}: {}", err.code, err.message),
                is_error: true,
            };
        }

        let result = resp.result.unwrap_or(json!({}));
        let is_error = result
            .get("isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Extract content text
        let content = if let Some(content_arr) = result.get("content").and_then(|v| v.as_array()) {
            content_arr
                .iter()
                .filter_map(|block| {
                    if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                        block.get("text").and_then(|t| t.as_str()).map(|s| s.to_string())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            result.to_string()
        };

        McpToolResult { content, is_error }
    }

    async fn send_request(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> Result<JsonRpcResponse, String> {
        let req = JsonRpcRequest::new(next_id(), method, params);
        let mut line = serde_json::to_string(&req)
            .map_err(|e| format!("serialize error: {e}"))?;
        line.push('\n');

        {
            let mut stdin = self.stdin.lock().await;
            stdin
                .write_all(line.as_bytes())
                .await
                .map_err(|e| format!("write error: {e}"))?;
            stdin.flush().await.map_err(|e| format!("flush error: {e}"))?;
        }

        let mut response_line = String::new();
        {
            let mut stdout = self.stdout.lock().await;
            stdout
                .read_line(&mut response_line)
                .await
                .map_err(|e| format!("read error: {e}"))?;
        }

        serde_json::from_str::<JsonRpcResponse>(&response_line)
            .map_err(|e| format!("parse error: {e} (data={response_line:?})"))
    }

    async fn send_notification(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> Result<(), String> {
        #[derive(serde::Serialize)]
        struct Notification {
            jsonrpc: &'static str,
            method: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            params: Option<Value>,
        }

        let notif = Notification {
            jsonrpc: "2.0",
            method: method.to_string(),
            params,
        };
        let mut line = serde_json::to_string(&notif)
            .map_err(|e| format!("serialize error: {e}"))?;
        line.push('\n');

        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| format!("write error: {e}"))?;
        stdin.flush().await.map_err(|e| format!("flush error: {e}"))?;

        Ok(())
    }
}

#[async_trait]
impl McpTransport for Mutex<McpClient> {
    async fn request(
        &self,
        method: &str,
        params: Option<Value>,
        cancel: &CancellationToken,
    ) -> CcResult<Value> {
        let mut guard = self.lock().await;
        // A hung MCP stdio child (e.g. waiting on filesystem I/O) would
        // otherwise block the tool loop forever. Race the child's response
        // against the user cancel — the child is killed on Drop of McpClient
        // when the manager shuts down, so abandoning the read is safe.
        let resp = tokio::select! {
            r = guard.send_request(method, params) => r.map_err(CcError::Other)?,
            _ = cancel.cancelled() => {
                return Err(CcError::Other(format!(
                    "MCP stdio '{}' cancelled during request",
                    guard.server_name
                )));
            }
        };
        if let Some(err) = resp.error {
            return Err(CcError::Other(format!(
                "MCP error {}: {}",
                err.code, err.message
            )));
        }
        Ok(resp.result.unwrap_or(Value::Null))
    }

    async fn notify(&self, method: &str, params: Option<Value>) -> CcResult<()> {
        let mut guard = self.lock().await;
        guard
            .send_notification(method, params)
            .await
            .map_err(CcError::Other)
    }

    async fn close(&self) -> CcResult<()> {
        // Child process will be killed when dropped
        Ok(())
    }
}
