use cc_api::{ApiClient, CreateMessageRequest, StreamDelta};
use cc_core::{
    CcError, CcResult, ContentBlock, MessageContent, MessageParam, Role, StopReason,
    SystemBlock, ToolResultBlock, ToolUseBlock,
};
use cc_hooks::{HookInput, HookRunner};
use cc_permissions::PermissionEngine;
use cc_session::Session;
use cc_tools::ToolResult;
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::permission_prompt::PromptDecision;
use crate::prompter::{PermissionPrompter, StdinPrompter};
use crate::tool_registry::ToolRegistry;

/// Maximum turns in a single `run_conversation` call.
const MAX_TURNS: usize = 50;

/// Options for a query run.
#[derive(Debug, Clone)]
pub struct QueryOptions {
    pub model: String,
    pub max_tokens: u32,
    /// Whether to auto-deny permission prompts (non-interactive sessions).
    pub non_interactive: bool,
    /// Bypass all permission checks.
    pub bypass_permissions: bool,
}

impl Default for QueryOptions {
    fn default() -> Self {
        QueryOptions {
            model: cc_core::models::DEFAULT.to_string(),
            max_tokens: 8192,
            non_interactive: false,
            bypass_permissions: false,
        }
    }
}

/// The main agentic query engine.
pub struct QueryEngine {
    api: ApiClient,
    tools: Arc<ToolRegistry>,
    permissions: PermissionEngine,
    hooks: HookRunner,
    session: Session,
    system_blocks: Vec<SystemBlock>,
    options: QueryOptions,
    prompter: Arc<dyn PermissionPrompter>,
    /// Set to `true` when auto-compact fires inside the most recent `run_turn`.
    /// The TUI reads this on `EngineDone` to render a compaction boundary in
    /// the transcript. Cleared at the start of every `run_turn`.
    compacted_last_turn: bool,
}

impl QueryEngine {
    pub fn new(
        api: ApiClient,
        tools: Arc<ToolRegistry>,
        permissions: PermissionEngine,
        hooks: HookRunner,
        session: Session,
        system_blocks: Vec<SystemBlock>,
        options: QueryOptions,
    ) -> Self {
        let non_interactive = options.non_interactive;
        QueryEngine {
            api,
            tools,
            permissions,
            hooks,
            session,
            system_blocks,
            options,
            prompter: Arc::new(StdinPrompter::new(non_interactive)),
            compacted_last_turn: false,
        }
    }

    /// Whether auto-compact fired during the most recently completed
    /// `run_turn`. Reset at the start of the next turn.
    pub fn compacted_last_turn(&self) -> bool {
        self.compacted_last_turn
    }

    /// Override the permission prompter (e.g. inject a TUI dialog prompter).
    pub fn with_prompter(mut self, prompter: Arc<dyn PermissionPrompter>) -> Self {
        self.prompter = prompter;
        self
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Read-only view of the active model id. Used by the TUI to render
    /// `/model` and `/config` without having to track it separately.
    pub fn model(&self) -> &str {
        &self.options.model
    }

    /// Switch the active model in place. The next `run_turn` call uses the new
    /// id; in-flight turns are not affected. Caller is responsible for passing
    /// a fully-qualified model id (use `cc_config::expand_model_alias`).
    pub fn set_model(&mut self, model: impl Into<String>) {
        self.options.model = model.into();
    }

    /// Run a complete conversation turn: add user message, loop until end_turn.
    /// Streams text deltas via `on_text` callback. Returns the final assistant text.
    pub async fn run_turn(
        &mut self,
        user_text: impl Into<String>,
        mut on_text: impl FnMut(&str),
        messages: &mut Vec<MessageParam>,
    ) -> CcResult<String> {
        // Clear per-turn flags before anything else.
        self.compacted_last_turn = false;

        // 1. Add user message
        let user_msg = MessageParam::user(user_text.into());
        self.session.append(&user_msg)?;
        messages.push(user_msg);

        // 2. Build tool definitions for the API
        let tool_defs = self.tools.definitions();

        let mut final_text = String::new();
        let mut turns = 0;

        loop {
            turns += 1;
            if turns > MAX_TURNS {
                return Err(CcError::Other(format!(
                    "exceeded max turns ({MAX_TURNS})"
                )));
            }

            debug!("query turn {turns}, messages={}", messages.len());

            // 3. Build API request
            let mut req = CreateMessageRequest::new(&self.options.model, messages.clone())
                .with_max_tokens(self.options.max_tokens);

            if !self.system_blocks.is_empty() {
                req = req.with_system(self.system_blocks.clone());
            }
            if !tool_defs.is_empty() {
                req = req.with_tools(tool_defs.clone());
            }

            // 4. Stream response
            let mut text_buf = String::new();
            let mut tool_use_blocks: Vec<ToolUseBlock> = Vec::new();
            let cancel = CancellationToken::new();

            let (message, usage) = self
                .api
                .complete_message(req, |delta| {
                    if let StreamDelta::Text(ref text) = delta {
                        text_buf.push_str(text);
                        on_text(text);
                    }
                }, &cancel)
                .await?;

            let input_tokens = usage.input_tokens;
            let stop_reason = message.stop_reason;

            // Collect tool_use blocks from the response
            for block in &message.content {
                if let ContentBlock::ToolUse(tu) = block {
                    tool_use_blocks.push(tu.clone());
                }
            }

            // Build assistant message from response
            let assistant_msg = MessageParam {
                role: Role::Assistant,
                content: MessageContent::Blocks(message.content.clone()),
            };
            self.session.append(&assistant_msg)?;
            messages.push(assistant_msg);

            if !text_buf.is_empty() {
                final_text = text_buf.clone();
            }

            // 5. Handle stop reason
            match stop_reason {
                Some(StopReason::ToolUse) if !tool_use_blocks.is_empty() => {
                    // Execute tools and build tool_result user message
                    let tool_results = self
                        .execute_tools(&tool_use_blocks, messages)
                        .await?;

                    let result_msg = MessageParam {
                        role: Role::User,
                        content: MessageContent::Blocks(
                            tool_results
                                .into_iter()
                                .map(ContentBlock::ToolResult)
                                .collect(),
                        ),
                    };
                    self.session.append(&result_msg)?;
                    messages.push(result_msg);

                    // Auto-compact check (§4.5: threshold = context_window - 13,000)
                    let threshold = compact_threshold();
                    if input_tokens > threshold {
                        debug!(
                            "auto-compact triggered: input_tokens={input_tokens} > threshold={threshold}"
                        );
                        compact_messages(messages);
                        self.compacted_last_turn = true;
                    }

                    // Continue loop
                }
                _ => {
                    // end_turn, max_tokens, or stop_sequence → done
                    break;
                }
            }
        }

        Ok(final_text)
    }

    /// Execute a batch of tool_use blocks, returning tool_result blocks.
    /// Read-only tools run concurrently; mutating tools run sequentially (§4.3).
    async fn execute_tools(
        &mut self,
        tool_use_blocks: &[ToolUseBlock],
        _messages: &[MessageParam],
    ) -> CcResult<Vec<ToolResultBlock>> {
        // Partition into read-only and mutating
        let (read_only, mutating): (Vec<_>, Vec<_>) = tool_use_blocks.iter().partition(|tu| {
            self.tools
                .get(&tu.name)
                .is_some_and(|t| t.is_read_only())
        });

        let mut results = Vec::with_capacity(tool_use_blocks.len());

        // Run read-only tools concurrently — pre-check permissions sequentially,
        // then execute the actual tool calls in parallel.
        if !read_only.is_empty() {
            let mut authorized: Vec<(&ToolUseBlock, Arc<dyn cc_tools::Tool>)> = Vec::new();
            for tu in &read_only {
                match self.check_tool_permissions(tu).await {
                    Ok(tool) => authorized.push((tu, tool)),
                    Err(result) => results.push(result),
                }
            }

            if !authorized.is_empty() {
                let futures: Vec<_> = authorized
                    .into_iter()
                    .map(|(tu, tool)| {
                        let cancel = CancellationToken::new();
                        async move {
                            let result: ToolResult =
                                match tool.execute(tu.input.clone(), &cancel).await {
                                    Ok(r) => r,
                                    Err(e) => ToolResult::error(format!("Tool execution error: {e}")),
                                };
                            ToolResultBlock {
                                tool_use_id: tu.id.clone(),
                                content: Some(Value::String(result.content)),
                                is_error: if result.is_error { Some(true) } else { None },
                            }
                        }
                    })
                    .collect();
                results.extend(futures::future::join_all(futures).await);
            }
        }

        // Run mutating tools sequentially
        for tu in &mutating {
            results.push(self.execute_one_tool(tu).await);
        }

        // Re-sort to match original tool_use order
        results.sort_by_key(|r| {
            tool_use_blocks
                .iter()
                .position(|tu| tu.id == r.tool_use_id)
                .unwrap_or(usize::MAX)
        });

        Ok(results)
    }

    /// Check hooks and permissions for a tool. Returns the tool Arc on success,
    /// or a ToolResultBlock error on failure.
    async fn check_tool_permissions(
        &mut self,
        tu: &ToolUseBlock,
    ) -> Result<Arc<dyn cc_tools::Tool>, ToolResultBlock> {
        let tool_name = &tu.name;
        let input = &tu.input;

        // --- PreToolUse hook ---
        let hook_input = HookInput {
            session_id: self.session.id.clone(),
            transcript_path: None,
            cwd: std::env::current_dir()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
            permission_mode: None,
            hook_event_name: "PreToolUse".into(),
            tool_name: Some(tool_name.clone()),
            tool_input: Some(input.clone()),
            tool_use_id: Some(tu.id.clone()),
            tool_response: None,
            source: None,
            model: None,
            message: None,
            agent_id: None,
        };
        let cancel = CancellationToken::new();
        let hook_result = self.hooks.run("PreToolUse", &hook_input, &cancel).await;
        if hook_result.blocked {
            let msg = hook_result
                .block_message
                .unwrap_or_else(|| "blocked by hook".into());
            return Err(tool_result_error(&tu.id, format!("Blocked by hook: {msg}")));
        }

        // --- Permission check ---
        if !self.options.bypass_permissions {
            let result = self.permissions.check(tool_name, input);
            use cc_core::PermissionBehavior;
            match result.behavior {
                PermissionBehavior::Deny => {
                    return Err(tool_result_error(
                        &tu.id,
                        format!("Permission denied for tool '{tool_name}'"),
                    ));
                }
                PermissionBehavior::Ask => {
                    let decision = self.prompter.prompt(tool_name, input).await;
                    match decision {
                        PromptDecision::Deny => {
                            return Err(tool_result_error(
                                &tu.id,
                                format!("Permission denied for tool '{tool_name}'"),
                            ));
                        }
                        PromptDecision::AllowAlways => {
                            self.permissions.add_session_allow(tool_name);
                        }
                        PromptDecision::Allow => {}
                    }
                }
                PermissionBehavior::Allow => {}
            }
        }

        // --- Find tool ---
        self.tools
            .get_arc(tool_name)
            .cloned()
            .ok_or_else(|| tool_result_error(&tu.id, format!("Unknown tool: {tool_name}")))
    }

    async fn execute_one_tool(&mut self, tu: &ToolUseBlock) -> ToolResultBlock {
        debug!("tool_use: {}", tu.name);

        let tool = match self.check_tool_permissions(tu).await {
            Ok(t) => t,
            Err(result) => return result,
        };

        let cancel = CancellationToken::new();
        let result: ToolResult = match tool.execute(tu.input.clone(), &cancel).await {
            Ok(r) => r,
            Err(e) => ToolResult::error(format!("Tool execution error: {e}")),
        };

        ToolResultBlock {
            tool_use_id: tu.id.clone(),
            content: Some(Value::String(result.content)),
            is_error: if result.is_error { Some(true) } else { None },
        }
    }
}

fn tool_result_error(tool_use_id: &str, message: impl Into<String>) -> ToolResultBlock {
    ToolResultBlock {
        tool_use_id: tool_use_id.to_string(),
        content: Some(Value::String(message.into())),
        is_error: Some(true),
    }
}

/// Auto-compact threshold (§4.5).
/// = effective_context_window - 13,000
/// Default effective_context_window = 200,000 (claude-sonnet-4-6).
fn compact_threshold() -> u32 {
    let context_window: u32 = std::env::var("CLAUDE_CODE_MAX_CONTEXT_TOKENS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200_000);
    context_window.saturating_sub(13_000)
}

/// Simple compaction: keep only the first user message and the last N messages.
/// Used both by the engine's auto-compact path (when input tokens cross
/// `AUTO_COMPACT_TOKEN_THRESHOLD`) and by the `/compact` slash command in the
/// TUI host. This is a basic implementation that satisfies the exit criterion.
pub fn compact_messages(messages: &mut Vec<MessageParam>) {
    const KEEP_RECENT: usize = 20;
    if messages.len() <= KEEP_RECENT + 1 {
        return;
    }

    debug!("compacting {} messages → keeping last {KEEP_RECENT}", messages.len());

    // Keep first message (initial user message) + last KEEP_RECENT
    let first = messages[0].clone();
    let keep_from = messages.len().saturating_sub(KEEP_RECENT);
    let recent: Vec<MessageParam> = messages[keep_from..].to_vec();

    messages.clear();
    messages.push(first);

    // Add a system-style summary notice as a user message
    messages.push(MessageParam::user(
        "[Context compacted: earlier messages trimmed to stay within token limits]",
    ));

    messages.extend(recent);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: Role, text: &str) -> MessageParam {
        MessageParam {
            role,
            content: MessageContent::Text(text.to_string()),
        }
    }

    #[test]
    fn compact_messages_keeps_first_and_last_n() {
        // 50 messages → should collapse to: first + boundary marker + last 20.
        let mut msgs: Vec<MessageParam> = (0..50)
            .map(|i| {
                let role = if i % 2 == 0 { Role::User } else { Role::Assistant };
                msg(role, &format!("m{i}"))
            })
            .collect();
        compact_messages(&mut msgs);
        assert_eq!(msgs.len(), 22, "first + boundary + last 20 = 22");
        // First message preserved.
        if let MessageContent::Text(t) = &msgs[0].content {
            assert_eq!(t, "m0");
        } else {
            panic!("expected text content for first message");
        }
        // Second slot is the "[Context compacted: ...]" marker.
        if let MessageContent::Text(t) = &msgs[1].content {
            assert!(t.contains("Context compacted"), "marker text present");
        } else {
            panic!("expected text content for marker");
        }
        // Last message is the original last.
        if let MessageContent::Text(t) = &msgs[msgs.len() - 1].content {
            assert_eq!(t, "m49");
        } else {
            panic!("expected text content for last message");
        }
    }

    #[test]
    fn compact_messages_noop_when_short() {
        let mut msgs: Vec<MessageParam> = (0..10).map(|i| msg(Role::User, &format!("m{i}"))).collect();
        let before = msgs.clone();
        compact_messages(&mut msgs);
        assert_eq!(msgs.len(), before.len(), "no compaction when ≤ KEEP_RECENT + 1");
    }
}
