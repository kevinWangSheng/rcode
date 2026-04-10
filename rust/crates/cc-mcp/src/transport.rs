//! MCP transport abstraction (§9.1).

use async_trait::async_trait;
use cc_core::CcResult;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// Unified MCP transport interface.
#[async_trait]
pub trait McpTransport: Send + Sync {
    /// Send a JSON-RPC request and receive the response.
    async fn request(
        &self,
        method: &str,
        params: Option<Value>,
        cancel: &CancellationToken,
    ) -> CcResult<Value>;

    /// Send a JSON-RPC notification (no response expected).
    async fn notify(&self, method: &str, params: Option<Value>) -> CcResult<()>;

    /// Close the transport.
    async fn close(&self) -> CcResult<()>;
}
