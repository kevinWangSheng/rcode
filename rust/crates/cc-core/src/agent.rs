//! SubAgentRunner trait — breaks the cc-agents → cc-query → cc-tools → cc-agents cycle.
//!
//! cc-tools depends on cc-core (not cc-query), so AgentTool holds an
//! `Arc<dyn SubAgentRunner>`. cc-query implements this trait for QueryEngine.
//! main.rs wires the concrete impl into the tool at startup.

use crate::{CcResult, MessageParam};
use tokio_util::sync::CancellationToken;

/// Runs a sub-agent query loop given a prompt and returns the final text.
#[async_trait::async_trait]
pub trait SubAgentRunner: Send + Sync {
    /// Execute an agent turn with the given system prompt and user message.
    /// Returns the final assistant text after the agent loop completes.
    async fn run(
        &self,
        system: Option<String>,
        prompt: String,
        initial_messages: Vec<MessageParam>,
        cancel: CancellationToken,
    ) -> CcResult<String>;
}
