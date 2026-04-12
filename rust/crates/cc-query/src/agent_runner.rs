//! SubAgentRunner implementation for QueryEngine.
//!
//! Allows cc-tools AgentTool to spawn local sub-agents without creating a
//! circular dependency (cc-tools → cc-core::SubAgentRunner ← cc-query).

use std::sync::Arc;

use async_trait::async_trait;
use cc_api::ApiClient;
use cc_core::{CcResult, MessageParam, PermissionPrompter, SubAgentRunner, SystemBlock};
use cc_hooks::HookRunner;
use cc_permissions::PermissionEngine;
use cc_session::Session;
use tokio_util::sync::CancellationToken;

use crate::engine::{QueryEngine, QueryOptions};
use crate::tool_registry::ToolRegistry;

/// Factory that builds a fresh QueryEngine for each sub-agent invocation.
///
/// All fields are cheap-to-clone handles to shared state (Arc/Clone).
/// Each call to `run()` creates an isolated session so sub-agent transcripts
/// don't pollute the parent session.
pub struct SubAgentRunnerImpl {
    pub api: ApiClient,
    pub tools: Arc<ToolRegistry>,
    pub permissions: PermissionEngine,
    pub hooks: Arc<HookRunner>,
    pub system_blocks: Vec<SystemBlock>,
    pub options: QueryOptions,
    pub prompter: Arc<dyn PermissionPrompter>,
}

#[async_trait]
impl SubAgentRunner for SubAgentRunnerImpl {
    async fn run(
        &self,
        system: Option<String>,
        prompt: String,
        initial_messages: Vec<MessageParam>,
        cancel: CancellationToken,
    ) -> CcResult<String> {
        // Each sub-agent gets its own fresh session (isolated transcript).
        let session = Session::new().map_err(|e| {
            cc_core::CcError::Other(format!("sub-agent session create failed: {e}"))
        })?;

        // Build the sub-agent's system blocks: parent blocks + optional override.
        let mut sys_blocks = self.system_blocks.clone();
        if let Some(extra) = system {
            sys_blocks.push(cc_core::SystemBlock {
                kind: "text".into(),
                text: extra,
                cache_control: None,
            });
        }

        let mut engine = QueryEngine::new(
            self.api.clone(),
            self.tools.clone(),
            self.permissions.clone(),
            self.hooks.clone(),
            session,
            sys_blocks,
            self.options.clone(),
            self.prompter.clone(),
        );

        let mut messages = initial_messages;
        engine
            .run_turn(prompt, |_| {}, &mut messages, &cancel)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sub_agent_runner_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SubAgentRunnerImpl>();
    }
}
