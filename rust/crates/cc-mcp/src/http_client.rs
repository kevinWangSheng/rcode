//! MCP HTTP transport (Streamable HTTP, MCP 2025 spec).
//!
//! Each JSON-RPC request is POSTed to the server URL. The server responds with
//! either:
//!   - `Content-Type: application/json`  → a single JSON-RPC response object
//!   - `Content-Type: text/event-stream` → an SSE stream where each `data:` line
//!     contains a JSON-RPC message; we wait for the message whose `id` matches
//!     the request we sent.
//!
//! Notifications (no id) are POSTed and the server is expected to return 202.
//!
//! This is intentionally a thin slice — enough to satisfy `initialize`,
//! `tools/list`, and `tools/call` from a single `McpHttpClient`. We do not
//! maintain a long-lived SSE listener for server-to-client notifications; that
//! would belong in a follow-up if/when we need server-pushed tool list updates.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use cc_core::{CcError, CcResult};
use reqwest::{header, Client};
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::transport::McpTransport;
use crate::types::{JsonRpcRequest, JsonRpcResponse, McpTool, McpToolResult};

static REQUEST_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    REQUEST_ID.fetch_add(1, Ordering::SeqCst)
}

/// HTTP-based MCP client.
pub struct McpHttpClient {
    pub server_name: String,
    url: String,
    http: Client,
}

impl McpHttpClient {
    /// Build a client and run the `initialize` handshake against `url`.
    pub async fn connect(
        server_name: impl Into<String>,
        url: impl Into<String>,
    ) -> Result<Self, String> {
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| format!("failed to build http client: {e}"))?;

        let client = McpHttpClient {
            server_name: server_name.into(),
            url: url.into(),
            http,
        };

        let init_params = json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "claude-code", "version": "0.1.0"}
        });
        let resp = client
            .send_request("initialize", Some(init_params))
            .await
            .map_err(|e| format!("MCP HTTP initialize failed: {e}"))?;

        if let Some(err) = resp.error {
            return Err(format!("MCP HTTP initialize error: {} ({})", err.message, err.code));
        }

        debug!("MCP HTTP server '{}' initialized", client.server_name);

        // Best-effort initialized notification — many servers ignore it but the
        // spec says clients MUST send it.
        let _ = client
            .send_notification("notifications/initialized", None)
            .await;

        Ok(client)
    }

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

        for tool in &mut tools {
            tool.server_name = self.server_name.clone();
        }
        Ok(tools)
    }

    pub async fn call_tool(&mut self, tool_name: &str, arguments: Value) -> McpToolResult {
        let params = json!({"name": tool_name, "arguments": arguments});

        let resp = match self.send_request("tools/call", Some(params)).await {
            Ok(r) => r,
            Err(e) => {
                return McpToolResult {
                    content: format!("MCP HTTP request error: {e}"),
                    is_error: true,
                }
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

        let content = if let Some(arr) = result.get("content").and_then(|v| v.as_array()) {
            arr.iter()
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
        &self,
        method: &str,
        params: Option<Value>,
    ) -> Result<JsonRpcResponse, String> {
        let req_id = next_id();
        let req = JsonRpcRequest::new(req_id, method, params);

        let resp = self
            .http
            .post(&self.url)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .json(&req)
            .send()
            .await
            .map_err(|e| format!("http error: {e}"))?;

        if !resp.status().is_success() {
            return Err(format!("http {}: {}", resp.status(), resp.status().as_str()));
        }

        let content_type = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/json")
            .to_string();

        if content_type.starts_with("application/json") {
            let bytes = resp.bytes().await.map_err(|e| format!("read body: {e}"))?;
            return serde_json::from_slice::<JsonRpcResponse>(&bytes)
                .map_err(|e| format!("parse json-rpc: {e}"));
        }

        if content_type.starts_with("text/event-stream") {
            let body = resp.text().await.map_err(|e| format!("read sse: {e}"))?;
            return parse_sse_for_id(&body, req_id);
        }

        Err(format!("unexpected content-type: {content_type}"))
    }

    async fn send_notification(&self, method: &str, params: Option<Value>) -> Result<(), String> {
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
        let resp = self
            .http
            .post(&self.url)
            .header(header::CONTENT_TYPE, "application/json")
            .json(&notif)
            .send()
            .await
            .map_err(|e| format!("http error: {e}"))?;
        if !resp.status().is_success() && resp.status().as_u16() != 202 {
            return Err(format!("http {}", resp.status()));
        }
        Ok(())
    }
}

#[async_trait]
impl McpTransport for Mutex<McpHttpClient> {
    async fn request(
        &self,
        method: &str,
        params: Option<Value>,
        _cancel: &CancellationToken,
    ) -> CcResult<Value> {
        let guard = self.lock().await;
        let resp = guard
            .send_request(method, params)
            .await
            .map_err(CcError::Other)?;
        if let Some(err) = resp.error {
            return Err(CcError::Other(format!(
                "MCP error {}: {}",
                err.code, err.message
            )));
        }
        Ok(resp.result.unwrap_or(Value::Null))
    }

    async fn notify(&self, method: &str, params: Option<Value>) -> CcResult<()> {
        let guard = self.lock().await;
        guard
            .send_notification(method, params)
            .await
            .map_err(CcError::Other)
    }

    async fn close(&self) -> CcResult<()> {
        Ok(())
    }
}

/// Parse a Server-Sent Events body and return the first JSON-RPC response
/// whose `id` matches `target_id`. Tolerates multi-line `data:` payloads and
/// ignores `event:` / comment lines.
pub fn parse_sse_for_id(body: &str, target_id: u64) -> Result<JsonRpcResponse, String> {
    let mut buf = String::new();
    for line in body.lines().chain(std::iter::once("")) {
        if line.is_empty() {
            // Dispatch event
            if !buf.is_empty() {
                if let Ok(resp) = serde_json::from_str::<JsonRpcResponse>(buf.trim()) {
                    if resp.id == Some(target_id) {
                        return Ok(resp);
                    }
                }
                buf.clear();
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("data:") {
            if !buf.is_empty() {
                buf.push('\n');
            }
            buf.push_str(rest.trim_start());
        }
        // Ignore other SSE fields (event:, id:, retry:, comments).
    }
    Err(format!("no SSE response found for id={target_id}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_sse_response() {
        let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{\"ok\":true}}\n\n";
        let resp = parse_sse_for_id(body, 7).unwrap();
        assert_eq!(resp.id, Some(7));
        assert!(resp.result.is_some());
    }

    #[test]
    fn skips_unrelated_sse_events_and_picks_matching_id() {
        let body = "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/x\"}\n\ndata: {\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{}}\n\n";
        let resp = parse_sse_for_id(body, 3).unwrap();
        assert_eq!(resp.id, Some(3));
    }

    #[test]
    fn returns_error_when_id_not_present() {
        let body = "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n\n";
        assert!(parse_sse_for_id(body, 99).is_err());
    }
}
