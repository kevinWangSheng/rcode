//! MCP (Model Context Protocol) client.
//!
//! Implements JSON-RPC 2.0 over stdin/stdout (stdio) and HTTP transports.
//! Supports: initialize, tools/list, tools/call.
//! Per Phase 2 §9: transport trait, server lifecycle, tool adapter.

pub mod adapter;
pub mod client;
pub mod http_client;
pub mod manager;
pub mod transport;
pub mod types;

pub use adapter::{load_mcp_tools_from_config, McpToolAdapter};
pub use client::McpClient;
pub use http_client::{parse_sse_last_event_id, redact_headers, McpHttpClient};
pub use manager::{ConnectedServer, McpManager, McpServerState};
pub use transport::McpTransport;
pub use types::{McpTool, McpToolResult};
