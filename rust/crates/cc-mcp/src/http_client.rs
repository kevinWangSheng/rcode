//! MCP HTTP transport (Streamable HTTP, MCP 2025-03-26 spec).
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
//! Session management (MCP 2025-03-26 §Session Management):
//!   - If the server returns an `Mcp-Session-Id` header in the initialize
//!     response, the client MUST echo it on every subsequent request.
//!   - A 404 response to a request with a stored session ID means the session
//!     has expired; we clear the stored ID so the next call re-initializes.
//!   - `MCP-Protocol-Version` is sent on every request for servers that
//!     negotiate per-call.
//!
//! This is intentionally a thin slice — enough to satisfy `initialize`,
//! `tools/list`, and `tools/call` from a single `McpHttpClient`. We do not
//! maintain a long-lived server-to-client SSE listener (GET stream); that
//! would belong in a follow-up if/when we need server-pushed notifications.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use cc_core::{CcError, CcResult};
use reqwest::{header, Client, Response, StatusCode};
use serde_json::{json, Value};
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::transport::McpTransport;
use crate::types::{JsonRpcRequest, JsonRpcResponse, McpTool, McpToolResult};

static REQUEST_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    REQUEST_ID.fetch_add(1, Ordering::SeqCst)
}

/// Protocol version advertised in the `MCP-Protocol-Version` header.
/// Must match the `protocolVersion` field sent in the initialize payload.
const PROTOCOL_VERSION: &str = "2024-11-05";

/// Default per-request timeout. Matches the TS MCP client's 30s default.
const DEFAULT_TIMEOUT_MS: u64 = 30_000;

/// Versioned session state for an HTTP MCP connection.
///
/// Every outbound request `acquire()`s a snapshot `(id, version)`. When the
/// server returns 404 (session expired), the client calls
/// `reconnect_if_stale(snapshot_version)`:
///
///   - If the stored `version` has already advanced past the caller's
///     snapshot, another task has already reconnected; the caller just
///     retries with the fresh session.
///   - Otherwise, the caller bumps `version`, clears `id`, replays
///     `initialize`, and subsequent 404'd in-flight requests see the new
///     version and skip the reconnect.
///
/// The `reconnect_lock` serializes reconnect attempts so concurrent 404s
/// don't produce a thundering herd of parallel `initialize` calls.
struct McpSession {
    id: RwLock<Option<String>>,
    version: AtomicU64,
    reconnect_lock: Mutex<()>,
}

impl McpSession {
    fn new() -> Self {
        Self {
            id: RwLock::new(None),
            version: AtomicU64::new(0),
            reconnect_lock: Mutex::new(()),
        }
    }

    /// Snapshot of (session_id, version) for an outbound request.
    async fn acquire(&self) -> (Option<String>, u64) {
        let id = self.id.read().await.clone();
        let version = self.version.load(Ordering::Acquire);
        (id, version)
    }

    /// Current version number (for tests + debugging).
    fn current_version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    /// Update the stored session id, if it differs from what we have.
    /// Returns `true` if the value changed.
    async fn set_id(&self, sid: String) -> bool {
        let mut guard = self.id.write().await;
        if guard.as_deref() != Some(sid.as_str()) {
            *guard = Some(sid);
            true
        } else {
            false
        }
    }

    /// Clear the stored session id and return the previous value.
    async fn take_id(&self) -> Option<String> {
        self.id.write().await.take()
    }

    /// Current session id, if any.
    async fn id(&self) -> Option<String> {
        self.id.read().await.clone()
    }

    /// Bump the version + clear the id. Called under `reconnect_lock`.
    async fn invalidate(&self) {
        *self.id.write().await = None;
        self.version.fetch_add(1, Ordering::AcqRel);
    }
}

/// HTTP-based MCP client.
pub struct McpHttpClient {
    pub server_name: String,
    url: String,
    http: Client,
    /// Custom headers to send on every request — typically `Authorization:
    /// Bearer <token>` for OAuth-protected MCP servers. Sourced from the
    /// per-server `headers` map in settings.json.
    extra_headers: HashMap<String, String>,
    /// Session state (id + version + reconnect serializer). See
    /// [`McpSession`] for the 404-reconnect contract.
    session: McpSession,
    /// Most recent SSE `id:` seen on any response. Exposed for future
    /// resumable-stream work — not currently sent as a `Last-Event-Id`
    /// request header because we don't reconnect mid-request. Having it
    /// captured means the building block is ready when a long-lived
    /// server→client SSE listener lands.
    last_event_id: tokio::sync::Mutex<Option<String>>,
}

impl McpHttpClient {
    /// Build a client and run the `initialize` handshake against `url`.
    pub async fn connect(
        server_name: impl Into<String>,
        url: impl Into<String>,
    ) -> Result<Self, String> {
        Self::connect_with(server_name, url, HashMap::new(), DEFAULT_TIMEOUT_MS).await
    }

    /// Build a client with custom headers (e.g. `Authorization: Bearer …`)
    /// and run the `initialize` handshake.
    pub async fn connect_with_headers(
        server_name: impl Into<String>,
        url: impl Into<String>,
        extra_headers: HashMap<String, String>,
    ) -> Result<Self, String> {
        Self::connect_with(server_name, url, extra_headers, DEFAULT_TIMEOUT_MS).await
    }

    /// Full constructor with per-server timeout (ms). Tests and advanced
    /// callers use this directly; most callers should use `connect` or
    /// `connect_with_headers`.
    pub async fn connect_with(
        server_name: impl Into<String>,
        url: impl Into<String>,
        extra_headers: HashMap<String, String>,
        timeout_ms: u64,
    ) -> Result<Self, String> {
        let mut builder = Client::builder().timeout(Duration::from_millis(timeout_ms));
        if let Some(identity) = load_mtls_identity()? {
            builder = builder.identity(identity);
        }
        let http = builder
            .build()
            .map_err(|e| format!("failed to build http client: {e}"))?;

        let client = McpHttpClient {
            server_name: server_name.into(),
            url: url.into(),
            http,
            extra_headers,
            session: McpSession::new(),
            last_event_id: tokio::sync::Mutex::new(None),
        };

        // Redact sensitive values before logging. Matches the TS client
        // (src/services/mcp/client.ts `headersForLogging`).
        debug!(
            "MCP HTTP '{}' headers: {:?}",
            client.server_name,
            redact_headers(&client.extra_headers)
        );

        client
            .perform_initialize()
            .await
            .map_err(|e| format!("MCP HTTP initialize failed: {e}"))?;

        debug!(
            "MCP HTTP server '{}' initialized (session_id={:?})",
            client.server_name,
            client.session.id().await,
        );

        Ok(client)
    }

    /// Run the `initialize` JSON-RPC call + `notifications/initialized`
    /// follow-up. Factored out so both initial connect and the post-404
    /// reconnect path can share the handshake.
    ///
    /// Sends the `initialize` call via the non-retrying low-level path —
    /// a 404 during `initialize` means the server rejected the fresh
    /// handshake, which is a hard failure, not something to retry.
    async fn perform_initialize(&self) -> Result<(), String> {
        let init_params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": "claude-code", "version": "0.1.0"}
        });
        let resp = self
            .send_request_inner("initialize", Some(init_params))
            .await?;
        if let Some(err) = resp.error {
            return Err(format!(
                "MCP HTTP initialize error: {} ({})",
                err.message, err.code
            ));
        }

        // Best-effort initialized notification — many servers ignore it but
        // the spec says clients MUST send it.
        let _ = self
            .send_notification("notifications/initialized", None)
            .await;
        Ok(())
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
        let mut tools: Vec<McpTool> =
            serde_json::from_value(tools_val).map_err(|e| format!("failed to parse tools: {e}"))?;

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
            result.to_string()
        };

        McpToolResult { content, is_error }
    }

    /// Returns the currently-stored session ID, if any. Primarily for tests
    /// and debugging — production code doesn't need to read it directly.
    pub async fn session_id(&self) -> Option<String> {
        self.session.id().await
    }

    /// Returns the current session version counter. Every 404-driven
    /// reconnect bumps this. Exposed for tests to assert that concurrent
    /// 404s only produce one reconnect.
    pub fn session_version(&self) -> u64 {
        self.session.current_version()
    }

    /// Most recent SSE `id:` value observed across all responses. `None`
    /// until an SSE response carries an id. Exposed for tests + future
    /// resumable-stream work.
    pub async fn last_event_id(&self) -> Option<String> {
        self.last_event_id.lock().await.clone()
    }

    /// Send `DELETE <url>` with the current session ID header to tell the
    /// server we're terminating the session cleanly (MCP 2025-03-26
    /// §Session Management). Best-effort: servers MAY return 405 if they
    /// don't support explicit termination, in which case the session just
    /// times out on their side. Clears our local session_id either way.
    async fn terminate_session(&self) -> Result<(), String> {
        let Some(sid) = self.session.take_id().await else {
            return Ok(()); // No session to terminate.
        };

        let mut builder = self
            .http
            .delete(&self.url)
            .header("MCP-Protocol-Version", PROTOCOL_VERSION)
            .header("Mcp-Session-Id", &sid);
        for (k, v) in &self.extra_headers {
            builder = builder.header(k, v);
        }

        match builder.send().await {
            Ok(resp) => {
                let status = resp.status();
                // 200/202/204 = terminated; 405 = server doesn't support
                // explicit termination, that's fine; anything else is
                // surprising but non-fatal.
                if !status.is_success() && status.as_u16() != 405 {
                    debug!(
                        "MCP '{}' DELETE session returned {status} (non-fatal)",
                        self.server_name
                    );
                }
                Ok(())
            }
            Err(e) => {
                // Network error during shutdown shouldn't prevent exit.
                debug!("MCP '{}' DELETE session failed: {e}", self.server_name);
                Ok(())
            }
        }
    }

    /// Call a JSON-RPC method concurrently from multiple tasks sharing an
    /// `Arc<McpHttpClient>`. This is the path tests use to exercise the
    /// concurrent 404 race: unlike `list_tools` / `call_tool` (both `&mut
    /// self`), this takes `&self` so many in-flight requests can be in
    /// play at once — matching the real hazard the reconnect coordinator
    /// protects against.
    pub async fn call(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> Result<JsonRpcResponse, String> {
        self.send_request(method, params).await
    }

    /// Public send path — transparently handles a single 404 session-expiry
    /// reconnect + retry (MCP 2025-03-26 §Session Management). The retry
    /// is exactly-once: a second 404 (or any non-404 error on the retry)
    /// bubbles up to the caller unchanged.
    ///
    /// Concurrent in-flight requests that all race into a 404 are
    /// coordinated through [`McpSession`]: the first to see the 404 takes
    /// the reconnect lock, bumps the version, and replays `initialize`.
    /// The others observe the new version and skip the redundant
    /// reconnect — they just retry with the fresh session id.
    async fn send_request(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> Result<JsonRpcResponse, String> {
        // Capture a session snapshot before sending. If the response is
        // 404, the snapshot tells us (a) whether a retry even makes sense
        // — you can only lose a session you had — and (b) the version to
        // check against when coordinating with other concurrent 404s.
        let (snapshot_id, snapshot_version) = self.session.acquire().await;
        match self.send_request_inner(method, params.clone()).await {
            Ok(resp) => Ok(resp),
            Err(e) if is_session_expired_err(&e) => {
                // Only reconnect + retry if we *had* a live session when
                // the request went out. A 404 on a request that never
                // carried a session ID is either an uninitialized server
                // or a misconfigured URL — retrying would infinite-loop.
                if snapshot_id.is_none() {
                    return Err(e);
                }
                self.reconnect_if_stale(snapshot_version).await?;
                // Exactly one retry. Any error on this path (including a
                // second 404) surfaces to the caller as-is.
                match self.send_request_inner(method, params).await {
                    Err(e) if is_session_expired_err(&e) => {
                        // Second 404 in a row — the reconnect installed a
                        // fresh id but the server rejected it too. Clear
                        // that stale id so subsequent calls go through a
                        // fresh `initialize` rather than replaying the
                        // doomed session.
                        let _ = self.session.take_id().await;
                        Err(e)
                    }
                    other => other,
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Low-level send — does not retry on 404. Used by `send_request` (with
    /// a retry wrapper) and by `perform_initialize` (which must not retry
    /// because it's the thing the retry depends on).
    async fn send_request_inner(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> Result<JsonRpcResponse, String> {
        let req_id = next_id();
        let req = JsonRpcRequest::new(req_id, method, params);

        let resp = self.post(&req, /*is_notification=*/ false).await?;

        // 404 → surface a distinctive "session expired" marker and let the
        // outer `send_request` decide whether to reconnect + retry (it
        // only retries when the original request actually carried a
        // session ID). Clearing the stored id happens centrally in
        // `reconnect_if_stale` so concurrent 404s agree on a single
        // invalidate + reconnect under `McpSession::reconnect_lock`.
        if resp.status() == StatusCode::NOT_FOUND {
            return Err(session_expired_msg(&self.server_name));
        }

        if !resp.status().is_success() {
            return Err(format!(
                "http {}: {}",
                resp.status(),
                resp.status().as_str()
            ));
        }

        // Capture Mcp-Session-Id from the response (initialize sets this).
        // Header name is case-insensitive — reqwest normalizes to lowercase.
        if let Some(sid) = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
        {
            if self.session.set_id(sid.clone()).await {
                debug!("MCP '{}' session_id ← {}", self.server_name, sid);
            }
        }

        let content_type = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/json")
            .to_string();

        if content_type.starts_with("application/json") {
            let bytes = resp.bytes().await.map_err(|e| format!("read body: {e}"))?;
            let parsed: JsonRpcResponse =
                serde_json::from_slice(&bytes).map_err(|e| format!("parse json-rpc: {e}"))?;
            return lift_session_expired(&self.server_name, parsed);
        }

        if content_type.starts_with("text/event-stream") {
            let body = resp.text().await.map_err(|e| format!("read sse: {e}"))?;
            // Capture Last-Event-Id before parsing the JSON-RPC response so
            // the caller's error path (no matching id) still records what
            // we saw. Any non-empty `id:` line updates our stored value.
            if let Some(eid) = parse_sse_last_event_id(&body) {
                let mut guard = self.last_event_id.lock().await;
                *guard = Some(eid);
            }
            return lift_session_expired(&self.server_name, parse_sse_for_id(&body, req_id)?);
        }

        Err(format!("unexpected content-type: {content_type}"))
    }

    /// Coordinate reconnects after a 404.
    ///
    /// Multiple in-flight requests can all receive 404 simultaneously when
    /// a session expires. Each passes the version it captured *before*
    /// sending. Under the reconnect lock we check: has the stored version
    /// already advanced past that snapshot? If so, another task has
    /// already performed the `initialize` — we just return so the caller
    /// retries with the fresh session id. Otherwise we bump the version,
    /// clear the id, and replay `initialize` ourselves.
    async fn reconnect_if_stale(&self, snapshot_version: u64) -> Result<(), String> {
        let _guard = self.session.reconnect_lock.lock().await;
        if self.session.current_version() > snapshot_version {
            // Someone beat us to it. Nothing to do — the caller will retry
            // with the freshly-installed session id.
            return Ok(());
        }
        self.session.invalidate().await;
        debug!(
            "MCP '{}' session expired (404) — reconnecting (version now {})",
            self.server_name,
            self.session.current_version()
        );
        self.perform_initialize().await
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
        let resp = self.post(&notif, /*is_notification=*/ true).await?;
        if !resp.status().is_success() && resp.status().as_u16() != 202 {
            return Err(format!("http {}", resp.status()));
        }
        Ok(())
    }

    /// Issue the POST with all the common MCP headers plus our stored
    /// session ID. `is_notification` is cosmetic — the headers are the same,
    /// but the caller treats the response differently.
    async fn post<B: serde::Serialize + ?Sized>(
        &self,
        body: &B,
        _is_notification: bool,
    ) -> Result<Response, String> {
        let mut builder = self
            .http
            .post(&self.url)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .header("MCP-Protocol-Version", PROTOCOL_VERSION);

        if let Some(sid) = self.session.id().await {
            builder = builder.header("Mcp-Session-Id", sid);
        }

        for (k, v) in &self.extra_headers {
            builder = builder.header(k, v);
        }

        builder.json(body).send().await.map_err(|e| {
            if e.is_timeout() {
                format!(
                    "MCP '{}' request timed out (configured timeout applies to all requests; see `timeout_ms` in settings)",
                    self.server_name
                )
            } else if e.is_connect() {
                format!("MCP '{}' connect failed: {e}", self.server_name)
            } else {
                format!("MCP '{}' http error: {e}", self.server_name)
            }
        })
    }
}

#[async_trait]
impl McpTransport for Mutex<McpHttpClient> {
    async fn request(
        &self,
        method: &str,
        params: Option<Value>,
        cancel: &CancellationToken,
    ) -> CcResult<Value> {
        let guard = self.lock().await;
        // reqwest has a per-request timeout but no external abort — without
        // racing cancel, a 30s server stall blocks Ctrl+C for a full 30s.
        let resp = tokio::select! {
            r = guard.send_request(method, params) => r.map_err(CcError::Other)?,
            _ = cancel.cancelled() => {
                return Err(CcError::Other(format!(
                    "MCP http '{}' cancelled during request",
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
        let guard = self.lock().await;
        guard
            .send_notification(method, params)
            .await
            .map_err(CcError::Other)
    }

    async fn close(&self) -> CcResult<()> {
        let guard = self.lock().await;
        guard.terminate_session().await.map_err(CcError::Other)
    }
}

/// Return a copy of `headers` with any sensitive values replaced by
/// `[REDACTED]`. Used before debug-logging user-supplied headers so tokens
/// don't end up in log sinks. Key matching is case-insensitive and covers
/// the common header names that carry secrets.
pub fn redact_headers(headers: &HashMap<String, String>) -> HashMap<String, String> {
    headers
        .iter()
        .map(|(k, v)| {
            let lower = k.to_ascii_lowercase();
            let is_sensitive = lower == "authorization"
                || lower == "proxy-authorization"
                || lower == "x-api-key"
                || lower.contains("token")
                || lower.contains("secret");
            let value = if is_sensitive {
                "[REDACTED]".to_string()
            } else {
                v.clone()
            };
            (k.clone(), value)
        })
        .collect()
}

/// Error message used when a request hits a 404 on a session-bound call.
/// The outer `send_request` matches on this string to decide whether to
/// reconnect + retry. Keeping it behind a helper makes the string easier
/// to evolve without drifting between producer and consumer.
fn session_expired_msg(server_name: &str) -> String {
    format!("MCP session expired (404) for '{server_name}'; reconnecting")
}

/// JSON-RPC error code for "session has been closed" per the MCP
/// 2025-03-26 Streamable-HTTP transport spec. A server can return
/// this on a session-bound request instead of HTTP 404; the client
/// treats both paths identically (reconnect + retry-once).
const JSON_RPC_SESSION_EXPIRED_CODE: i64 = -32001;

/// Load a client-authentication identity (client certificate + private
/// key) for mutual-TLS against MCP servers that require it (P0 #10,
/// 2026-04-24 parity-gaps — plan §2 Decision 5 marks mTLS as an
/// in-scope cross-cutting feature). Reads `TLS_CERT` and `TLS_KEY` as
/// paths to PEM-encoded files. Returns:
///
///   - `Ok(None)` when neither env var is set (no mTLS requested).
///   - `Err(...)` when exactly one is set (misconfiguration —
///     fail loud so silent reverts to non-mTLS don't happen).
///   - `Err(...)` on read failure or malformed PEM.
///   - `Ok(Some(Identity))` with the loaded identity threaded into
///     the reqwest `Client::builder().identity(...)` pipeline.
///
/// The workspace's reqwest build uses `rustls-tls`, so we construct
/// the identity via `Identity::from_pem` on a concatenated buffer —
/// reqwest accepts a single PEM blob containing one CERTIFICATE
/// plus one PRIVATE KEY block.
fn load_mtls_identity() -> Result<Option<reqwest::Identity>, String> {
    let cert_path = std::env::var_os("TLS_CERT");
    let key_path = std::env::var_os("TLS_KEY");
    match (cert_path, key_path) {
        (None, None) => Ok(None),
        (Some(_), None) | (None, Some(_)) => Err(
            "mTLS misconfigured: both TLS_CERT and TLS_KEY must be set (or neither). \
             Set both to PEM-encoded file paths, or unset both to disable mTLS."
                .to_string(),
        ),
        (Some(cert), Some(key)) => {
            let cert_bytes = std::fs::read(&cert)
                .map_err(|e| format!("TLS_CERT: failed to read {}: {e}", cert.to_string_lossy()))?;
            let key_bytes = std::fs::read(&key)
                .map_err(|e| format!("TLS_KEY: failed to read {}: {e}", key.to_string_lossy()))?;
            let mut combined = Vec::with_capacity(cert_bytes.len() + key_bytes.len() + 1);
            combined.extend_from_slice(&cert_bytes);
            if !cert_bytes.ends_with(b"\n") {
                combined.push(b'\n');
            }
            combined.extend_from_slice(&key_bytes);
            reqwest::Identity::from_pem(&combined)
                .map(Some)
                .map_err(|e| format!("mTLS identity parse failed: {e}"))
        }
    }
}

/// Normalise "session expired" signals from the server into the same
/// error marker the HTTP 404 path raises. A server answering a
/// session-bound request with HTTP 200 + JSON-RPC `error.code ==
/// -32001` means "the id you carried is gone; please reinitialise"
/// — without lifting it here, `send_request` would bubble the success-
/// shaped JsonRpcResponse to the caller and the reconnect + retry
/// would never fire (P0 #11 tail, 2026-04-24 parity-gaps).
fn lift_session_expired(
    server_name: &str,
    resp: JsonRpcResponse,
) -> Result<JsonRpcResponse, String> {
    if let Some(err) = &resp.error {
        if err.code == JSON_RPC_SESSION_EXPIRED_CODE {
            return Err(session_expired_msg(server_name));
        }
    }
    Ok(resp)
}

/// Is `err` the distinctive session-expired marker we raise from
/// `send_request_inner`? Returning `true` tells the outer send path it's
/// safe to attempt a single reconnect + retry.
fn is_session_expired_err(err: &str) -> bool {
    err.contains("MCP session expired (404)")
}

/// Return the last non-empty SSE `id:` value observed in `body`, or `None`
/// if the stream carried no ids. Per the SSE spec, any `id:` line (including
/// `id:` alone) updates the reader's "last event ID" — we mirror that but
/// only remember non-empty values, since the only use for this is a future
/// `Last-Event-Id` reconnect header.
pub fn parse_sse_last_event_id(body: &str) -> Option<String> {
    let mut last = None;
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("id:") {
            let val = rest.trim();
            if !val.is_empty() {
                last = Some(val.to_string());
            }
        }
    }
    last
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
        let body =
            "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{\"ok\":true}}\n\n";
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

    #[test]
    fn parse_sse_last_event_id_returns_latest() {
        let body = "id: 1\ndata: {}\n\nid: 2\ndata: {}\n\nid: 3\ndata: {}\n\n";
        assert_eq!(parse_sse_last_event_id(body).as_deref(), Some("3"));
    }

    #[test]
    fn parse_sse_last_event_id_ignores_empty_and_returns_none() {
        let body = "data: {}\n\nevent: message\ndata: {}\n\n";
        assert_eq!(parse_sse_last_event_id(body), None);
    }

    #[test]
    fn parse_sse_last_event_id_empty_line_does_not_clear() {
        // Per the SSE spec an `id:` with an empty value should reset the
        // stored ID. For the Last-Event-Id reconnect use case we only care
        // about the last non-empty value — the reset semantics don't apply.
        let body = "id: 42\ndata: {}\n\nid:\ndata: {}\n\n";
        assert_eq!(parse_sse_last_event_id(body).as_deref(), Some("42"));
    }

    /// Shared mutex for all mTLS tests in this binary — `TLS_CERT` /
    /// `TLS_KEY` are process-global env vars, so without a single
    /// shared lock the three tests race each other under cargo's
    /// default parallel runner. Each test holds the guard for its
    /// whole body and drops on return.
    fn mtls_env_lock() -> std::sync::MutexGuard<'static, ()> {
        use std::sync::{Mutex, OnceLock};
        static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("mTLS env lock poisoned")
    }

    /// P0 #10: mTLS loader refuses a half-configured setup. Setting
    /// TLS_CERT without TLS_KEY (or vice versa) is almost always a
    /// typo / forgotten env var; silently falling back to no-mTLS
    /// would mean the first connection goes out unauthenticated.
    #[test]
    fn mtls_loader_refuses_partial_env() {
        let _guard = mtls_env_lock();

        std::env::set_var("TLS_CERT", "/tmp/does-not-exist.pem");
        std::env::remove_var("TLS_KEY");
        let err = load_mtls_identity().unwrap_err();
        assert!(err.contains("both TLS_CERT and TLS_KEY"), "got: {err}");

        std::env::remove_var("TLS_CERT");
        std::env::set_var("TLS_KEY", "/tmp/does-not-exist.key");
        let err = load_mtls_identity().unwrap_err();
        assert!(err.contains("both TLS_CERT and TLS_KEY"), "got: {err}");

        std::env::remove_var("TLS_KEY");
    }

    /// P0 #10: neither env set → no mTLS, clean `Ok(None)`.
    #[test]
    fn mtls_loader_returns_none_when_unconfigured() {
        let _guard = mtls_env_lock();
        std::env::remove_var("TLS_CERT");
        std::env::remove_var("TLS_KEY");
        assert!(load_mtls_identity().unwrap().is_none());
    }

    /// P0 #10: garbage PEM → parse error surfaces, we don't silently
    /// build a clientless identity.
    #[test]
    fn mtls_loader_rejects_malformed_pem() {
        use std::io::Write;
        let _guard = mtls_env_lock();

        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("cert.pem");
        let key = dir.path().join("key.pem");
        let mut f = std::fs::File::create(&cert).unwrap();
        writeln!(f, "not a pem").unwrap();
        let mut f = std::fs::File::create(&key).unwrap();
        writeln!(f, "also not a pem").unwrap();

        std::env::set_var("TLS_CERT", &cert);
        std::env::set_var("TLS_KEY", &key);
        let err = load_mtls_identity().unwrap_err();
        assert!(err.contains("identity parse failed"), "got: {err}");
        std::env::remove_var("TLS_CERT");
        std::env::remove_var("TLS_KEY");
    }

    #[test]
    fn lift_session_expired_converts_minus_32001_to_reconnect_marker() {
        let resp = JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id: Some(7),
            result: None,
            error: Some(crate::types::JsonRpcError {
                code: JSON_RPC_SESSION_EXPIRED_CODE,
                message: "Session has been closed".into(),
                data: None,
            }),
        };
        let err = lift_session_expired("fs", resp).unwrap_err();
        assert!(
            is_session_expired_err(&err),
            "lifted error must match is_session_expired_err: {err}"
        );
    }

    #[test]
    fn lift_session_expired_passes_through_other_errors() {
        let resp = JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id: Some(7),
            result: None,
            error: Some(crate::types::JsonRpcError {
                code: -32601,
                message: "method not found".into(),
                data: None,
            }),
        };
        let ok = lift_session_expired("fs", resp).unwrap();
        assert_eq!(ok.error.as_ref().map(|e| e.code), Some(-32601));
    }

    #[test]
    fn lift_session_expired_passes_through_success() {
        let resp = JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id: Some(7),
            result: Some(serde_json::json!({"ok": true})),
            error: None,
        };
        assert!(lift_session_expired("fs", resp).is_ok());
    }

    #[test]
    fn redact_headers_masks_sensitive_keys_case_insensitive() {
        let mut h = HashMap::new();
        h.insert("Authorization".into(), "Bearer abc".into());
        h.insert("X-API-Key".into(), "secret123".into());
        h.insert("x-session-token".into(), "tok".into());
        h.insert("Proxy-Authorization".into(), "Basic zzz".into());
        h.insert("User-Agent".into(), "claude-code/0.1".into());
        h.insert("X-Trace-Id".into(), "req-42".into());

        let out = redact_headers(&h);
        assert_eq!(out.get("Authorization").unwrap(), "[REDACTED]");
        assert_eq!(out.get("X-API-Key").unwrap(), "[REDACTED]");
        assert_eq!(out.get("x-session-token").unwrap(), "[REDACTED]");
        assert_eq!(out.get("Proxy-Authorization").unwrap(), "[REDACTED]");
        // Non-sensitive keys pass through unchanged.
        assert_eq!(out.get("User-Agent").unwrap(), "claude-code/0.1");
        assert_eq!(out.get("X-Trace-Id").unwrap(), "req-42");
    }
}
