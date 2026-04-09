//! MCP (Model Context Protocol) stdio client.
//!
//! Implements JSON-RPC 2.0 over stdin/stdout of a child process.
//! Supports: initialize, tools/list, tools/call.

pub mod adapter;
pub mod client;
pub mod http_client;
pub mod types;

pub use adapter::{load_mcp_tools_from_config, McpToolAdapter, McpTransport};
pub use client::McpClient;
pub use http_client::McpHttpClient;
pub use types::{McpTool, McpToolResult};
