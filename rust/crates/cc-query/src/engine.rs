use cc_api::{ApiClient, CreateMessageRequest};
use cc_core::{
    CcError, CcResult, ContentBlock, MessageContent, MessageParam, Role, StopReason,
    SystemBlock, ToolResultBlock, ToolUseBlock,
};
use cc_hooks::{HookInput, HookOutcome, HookRunner};
use cc_permissions::PermissionEngine;
use cc_session::Session;
use cc_tools::{tool_definition, Tool, ToolResult};
use serde_json::Value;
use std::sync::Arc;
use tracing::debug;

use crate::permission_prompt::{prompt_for_permission, PromptDecision};

/// Context window token threshold for auto-compact.
/// When `input_tokens` exceeds this, old messages are trimmed.
const AUTO_COMPACT_TOKEN_THRESHOLD: u32 = 80_000;

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
    tools: Vec<Arc<dyn Tool>>,
    permissions: PermissionEngine,
    hooks: HookRunner,
    session: Session,
    system_blocks: Vec<SystemBlock>,
    options: QueryOptions,
}

impl QueryEngine {
    pub fn new(
        api: ApiClient,
        tools: Vec<Arc<dyn Tool>>,
        permissions: PermissionEngine,
        hooks: HookRunner,
        session: Session,
        system_blocks: Vec<SystemBlock>,
        options: QueryOptions,
    ) -> Self {
        QueryEngine {
            api,
            tools,
            permissions,
            hooks,
            session,
            system_blocks,
            options,
        }
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Run a complete conversation turn: add user message, loop until end_turn.
    /// Streams text deltas via `on_text` callback. Returns the final assistant text.
    pub async fn run_turn(
        &mut self,
        user_text: impl Into<String>,
        mut on_text: impl FnMut(&str),
        messages: &mut Vec<MessageParam>,
    ) -> CcResult<String> {
        // 1. Add user message
        let user_msg = MessageParam::user(user_text.into());
        self.session.append(&user_msg)?;
        messages.push(user_msg);

        // 2. Build tool definitions for the API
        let tool_defs: Vec<_> = self.tools.iter().map(|t| tool_definition(t.as_ref())).collect();

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

            let message = self
                .api
                .complete_message(req, |delta| {
                    text_buf.push_str(delta);
                    on_text(delta);
                })
                .await?;

            let input_tokens = message.usage.input_tokens;
            let stop_reason = message.stop_reason.clone();

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

                    // Auto-compact check
                    if input_tokens > AUTO_COMPACT_TOKEN_THRESHOLD {
                        debug!(
                            "auto-compact triggered: input_tokens={input_tokens} > threshold={AUTO_COMPACT_TOKEN_THRESHOLD}"
                        );
                        compact_messages(messages);
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
    async fn execute_tools(
        &mut self,
        tool_use_blocks: &[ToolUseBlock],
        _messages: &[MessageParam],
    ) -> CcResult<Vec<ToolResultBlock>> {
        let mut results = Vec::new();

        for tu in tool_use_blocks {
            let result = self.execute_one_tool(tu).await;
            results.push(result);
        }

        Ok(results)
    }

    async fn execute_one_tool(&mut self, tu: &ToolUseBlock) -> ToolResultBlock {
        let tool_name = &tu.name;
        let input = &tu.input;

        debug!("tool_use: {tool_name}");

        // --- PreToolUse hook ---
        let hook_input = HookInput {
            event: "PreToolUse",
            tool_name,
            tool_input: input,
            session_id: Some(&self.session.id),
        };
        match self.hooks.run("PreToolUse", &hook_input).await {
            HookOutcome::Block(msg) => {
                return tool_result_error(&tu.id, format!("Blocked by hook: {msg}"));
            }
            HookOutcome::Failed { kind, detail } => {
                debug!("PreToolUse hook failed (non-blocking) [{kind}]: {detail}");
            }
            HookOutcome::Ok => {}
        }

        // --- Permission check ---
        if !self.options.bypass_permissions {
            let behavior = self.permissions.check(tool_name, input);
            use cc_core::PermissionBehavior;
            match behavior {
                PermissionBehavior::Deny => {
                    return tool_result_error(
                        &tu.id,
                        format!("Permission denied for tool '{tool_name}'"),
                    );
                }
                PermissionBehavior::Ask => {
                    let decision = prompt_for_permission(
                        tool_name,
                        input,
                        &mut self.permissions,
                        self.options.non_interactive,
                    )
                    .await;
                    match decision {
                        PromptDecision::Deny => {
                            return tool_result_error(
                                &tu.id,
                                format!("Permission denied for tool '{tool_name}'"),
                            );
                        }
                        PromptDecision::Allow | PromptDecision::AllowAlways => {}
                    }
                }
                PermissionBehavior::Allow => {}
            }
        }

        // --- Find and execute tool ---
        let tool = self.tools.iter().find(|t| t.name() == tool_name).cloned();

        match tool {
            None => tool_result_error(&tu.id, format!("Unknown tool: {tool_name}")),
            Some(t) => {
                let result: ToolResult = match t.execute(input.clone()).await {
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
    }
}

fn tool_result_error(tool_use_id: &str, message: impl Into<String>) -> ToolResultBlock {
    ToolResultBlock {
        tool_use_id: tool_use_id.to_string(),
        content: Some(Value::String(message.into())),
        is_error: Some(true),
    }
}

/// Simple auto-compact: keep only the first user message and the last N messages.
/// This is a basic implementation that satisfies the exit criterion.
fn compact_messages(messages: &mut Vec<MessageParam>) {
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
