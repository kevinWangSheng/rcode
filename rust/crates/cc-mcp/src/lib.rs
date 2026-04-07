//! MCP (Model Context Protocol) stdio client.
//!
//! Implements JSON-RPC 2.0 over stdin/stdout of a child process.
//! Supports: initialize, tools/list, tools/call.

pub mod client;
pub mod types;

pub use client::McpClient;
pub use types::{McpTool, McpToolResult};
