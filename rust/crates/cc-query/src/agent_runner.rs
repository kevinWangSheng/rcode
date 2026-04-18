//! SubAgentRunner implementation for QueryEngine.
//!
//! Allows cc-tools AgentTool to spawn local sub-agents without creating a
//! circular dependency (cc-tools → cc-core::SubAgentRunner ← cc-query).

use std::sync::Arc;

use async_trait::async_trait;
use cc_api::ApiClient;
use cc_core::{CcResult, MessageParam, PermissionPrompter, SubAgentRunner, SystemBlock};
use cc_hooks::{HookInput, HookRunner};
use cc_permissions::PermissionEngine;
use cc_session::Session;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

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

        // Stable id for this sub-agent lifecycle — referenced by SubagentStart
        // hook results (via `agent_id` in HookInput) so hooks can scope their
        // effects per subagent. Frontmatter hooks in TS are keyed off this.
        let agent_id = Uuid::new_v4().to_string();

        // Fire SubagentStart — hooks may inject additional_context that we
        // prepend as a user message (consistent with TS `runAgent.ts`).
        let start_input = HookInput::base(session.id.clone(), "SubagentStart")
            .with_transcript_path(session.transcript_path().to_string_lossy())
            .with_model(self.options.model.clone())
            .with_agent_id(agent_id.clone());
        let start_result = self.hooks.run("SubagentStart", &start_input, &cancel).await;

        // Prepend SubagentStart's injected contexts as a user message, matching
        // the TS attachment message shape for hook_additional_context.
        let mut messages = initial_messages;
        if !start_result.additional_contexts.is_empty() {
            let joined = start_result.additional_contexts.join("\n\n");
            messages.insert(0, MessageParam::user(joined));
        }

        // Build the sub-agent's system blocks: parent blocks + optional override.
        let mut sys_blocks = self.system_blocks.clone();
        if let Some(extra) = system {
            sys_blocks.push(cc_core::SystemBlock::text(extra));
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

        let result = engine
            .run_turn(prompt, |_| {}, &mut messages, &cancel)
            .await;

        // Fire SubagentStop regardless of turn outcome — mirrors TS behavior
        // where Stop hooks run in the agent's finally path. We don't act on
        // the return value here (no re-entry loop for subagents in Rust yet).
        let mut stop_input = HookInput::base(start_input.session_id.clone(), "SubagentStop")
            .with_model(self.options.model.clone())
            .with_agent_id(agent_id)
            .with_stop_hook_active(true)
            .with_last_assistant_message(result.as_ref().ok().cloned());
        // Reuse the start-input's transcript path (and its cwd snapshot) so
        // start/stop carry the same file reference even if cwd has shifted
        // underneath us since SubagentStart.
        stop_input.transcript_path = start_input.transcript_path.clone();
        stop_input.cwd = start_input.cwd.clone();
        let _ = self.hooks.run("SubagentStop", &stop_input, &cancel).await;

        result
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
