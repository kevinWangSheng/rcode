use cc_api::{ApiClient, CreateMessageRequest, StreamDelta, UsageTracker};
use cc_core::{
    AppEvent, CcError, CcResult, ContentBlock, MessageContent, MessageParam, PermissionBehavior,
    PermissionPrompter, PromptDecision, Role, StopReason, SystemBlock, ToolResultBlock,
    ToolUseBlock,
};
use cc_hooks::{HookInput, HookRunner};
use cc_permissions::PermissionEngine;
use cc_session::Session;
use cc_tools::ToolResult;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::debug;

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
///
/// ## System-prompt cache-tier contract
///
/// Callers passing `system_blocks` to `QueryEngine::new` MUST already
/// three-tier tag them per `RUST_REWRITE_PLAN.md` §3:
///
/// - attribution → `cache_control = None`
/// - static instruction → `CacheControl::ephemeral_global()`
/// - git / memory / dynamic → `CacheControl::ephemeral_org()`
///
/// The engine itself does not construct `SystemBlock`s; it forwards them as
/// received. If the agent-runner memory path ever grows a block-emission
/// site here, use `CacheControl::ephemeral_org()` for memory content.
pub struct QueryEngine {
    api: ApiClient,
    tools: Arc<ToolRegistry>,
    permissions: PermissionEngine,
    hooks: Arc<HookRunner>,
    session: Session,
    system_blocks: Vec<SystemBlock>,
    options: QueryOptions,
    prompter: Arc<dyn PermissionPrompter>,
    usage: UsageTracker,
    events_tx: Option<mpsc::Sender<AppEvent>>,
    /// Set to `true` when auto-compact fires inside the most recent `run_turn`.
    compacted_last_turn: bool,
}

impl QueryEngine {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        api: ApiClient,
        tools: Arc<ToolRegistry>,
        permissions: PermissionEngine,
        hooks: Arc<HookRunner>,
        session: Session,
        system_blocks: Vec<SystemBlock>,
        options: QueryOptions,
        prompter: Arc<dyn PermissionPrompter>,
    ) -> Self {
        QueryEngine {
            api,
            tools,
            permissions,
            hooks,
            session,
            system_blocks,
            options,
            prompter,
            usage: UsageTracker::default(),
            events_tx: None,
            compacted_last_turn: false,
        }
    }

    /// Set the TUI event channel for emitting `AppEvent`s.
    pub fn with_events(mut self, tx: mpsc::Sender<AppEvent>) -> Self {
        self.events_tx = Some(tx);
        self
    }

    /// Whether auto-compact fired during the most recently completed
    /// `run_turn`. Reset at the start of the next turn.
    pub fn compacted_last_turn(&self) -> bool {
        self.compacted_last_turn
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Read-only access to the cumulative usage tracker.
    pub fn usage(&self) -> &UsageTracker {
        &self.usage
    }

    /// Read-only view of the active model id.
    pub fn model(&self) -> &str {
        &self.options.model
    }

    /// Switch the active model in place.
    pub fn set_model(&mut self, model: impl Into<String>) {
        self.options.model = model.into();
    }

    /// Emit an event to the TUI if a channel is connected.
    async fn emit(&self, event: AppEvent) {
        if let Some(tx) = &self.events_tx {
            let _ = tx.send(event).await;
        }
    }

    /// Run a complete conversation turn: add user message, loop until end_turn.
    /// Streams text deltas via `on_text` callback. Returns the final assistant text.
    pub async fn run_turn(
        &mut self,
        user_text: impl Into<String>,
        mut on_text: impl FnMut(&str),
        messages: &mut Vec<MessageParam>,
        cancel: &CancellationToken,
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

            let events_tx = self.events_tx.clone();
            let (message, usage) = self
                .api
                .complete_message(req, |delta| {
                    match &delta {
                        StreamDelta::Text(ref text) => {
                            text_buf.push_str(text);
                            on_text(text);
                            if let Some(tx) = &events_tx {
                                let _ = tx.try_send(AppEvent::StreamDelta(text.clone()));
                            }
                        }
                        StreamDelta::Thinking(ref thinking) => {
                            if let Some(tx) = &events_tx {
                                let _ = tx.try_send(AppEvent::StreamThinking(thinking.clone()));
                            }
                        }
                        StreamDelta::ToolUseStart { id, name } => {
                            if let Some(tx) = &events_tx {
                                let _ = tx.try_send(AppEvent::StreamToolUse(ToolUseBlock {
                                    id: id.clone(),
                                    name: name.clone(),
                                    input: Value::Null,
                                }));
                            }
                        }
                        StreamDelta::InputJsonDelta(_) => {}
                    }
                }, cancel)
                .await?;

            // §4 contract: if streaming was interrupted (cancel fired), save partial
            // text with interrupt marker before propagating cancellation.
            if cancel.is_cancelled() && !text_buf.is_empty() {
                let mut interrupted_content = message.content.clone();
                interrupted_content.push(ContentBlock::text(
                    "\n[Interrupted by user]",
                ));
                let partial_msg = MessageParam {
                    role: Role::Assistant,
                    content: MessageContent::Blocks(interrupted_content),
                };
                self.session.append(&partial_msg)?;
                messages.push(partial_msg);
                return Err(CcError::Cancelled);
            }

            // Record usage
            self.usage.record(&usage);

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
                        .execute_tools(&tool_use_blocks, cancel)
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
                    if self.usage.last_input_tokens() > threshold {
                        debug!(
                            "auto-compact triggered: input_tokens={} > threshold={threshold}",
                            self.usage.last_input_tokens()
                        );
                        compact_messages(messages);
                        self.compacted_last_turn = true;
                        self.emit(AppEvent::CompactBoundary).await;
                    }

                    // Continue loop
                }
                _ => {
                    // §5.1: Fire Stop hook before concluding the turn.
                    // Exit 2 → stderr → model, continue conversation.
                    let stop_input = HookInput {
                        session_id: self.session.id.clone(),
                        transcript_path: Some(
                            self.session.transcript_path().to_string_lossy().to_string(),
                        ),
                        cwd: std::env::current_dir()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string(),
                        permission_mode: None,
                        hook_event_name: "Stop".to_string(),
                        tool_name: None,
                        tool_input: None,
                        tool_use_id: None,
                        tool_response: None,
                        source: None,
                        model: Some(self.options.model.clone()),
                        message: None,
                        agent_id: None,
                        stop_hook_active: Some(true),
                        last_assistant_message: if final_text.is_empty() {
                            None
                        } else {
                            Some(final_text.clone())
                        },
                    };
                    let stop_result = self.hooks.run("Stop", &stop_input, cancel).await;
                    if stop_result.blocked {
                        // Inject block message as user turn and continue loop
                        let block_msg = stop_result.block_message.unwrap_or_default();
                        debug!("Stop hook blocked: continuing conversation with model");
                        let continue_msg = MessageParam::user(block_msg);
                        self.session.append(&continue_msg)?;
                        messages.push(continue_msg);
                        // loop continues — don't break or emit TurnComplete yet
                    } else {
                        self.emit(AppEvent::TurnComplete { usage }).await;
                        break;
                    }
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
        cancel: &CancellationToken,
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
                match self.check_tool_permissions(tu, cancel).await {
                    Ok(tool) => authorized.push((tu, tool)),
                    Err(result) => results.push(result),
                }
            }

            if !authorized.is_empty() {
                let events_tx = self.events_tx.clone();
                let hooks = Arc::clone(&self.hooks);
                let session_id = self.session.id.clone();
                let futures: Vec<_> = authorized
                    .into_iter()
                    .map(|(tu, tool)| {
                        let cancel = cancel.child_token();
                        let events_tx = events_tx.clone();
                        let hooks = Arc::clone(&hooks);
                        let session_id = session_id.clone();
                        let tu = tu.clone();
                        async move {
                            // Emit ToolStart
                            if let Some(tx) = &events_tx {
                                let _ = tx.send(AppEvent::ToolStart {
                                    name: tu.name.clone(),
                                    input: tu.input.clone(),
                                }).await;
                            }

                            let result: ToolResult =
                                match tool.execute(tu.input.clone(), &cancel).await {
                                    Ok(r) => r,
                                    Err(e) => ToolResult::error(format!("Tool execution error: {e}")),
                                };

                            let tool_result_block = ToolResultBlock {
                                tool_use_id: tu.id.clone(),
                                content: Some(Value::String(result.content.clone())),
                                is_error: if result.is_error { Some(true) } else { None },
                            };

                            // Emit ToolEnd
                            if let Some(tx) = &events_tx {
                                let _ = tx.send(AppEvent::ToolEnd {
                                    name: tu.name.clone(),
                                    result: result.clone(),
                                }).await;
                            }

                            // PostToolUse hook
                            run_post_tool_hook(
                                &hooks, &session_id, &tu, &tool_result_block, &cancel,
                            ).await;

                            tool_result_block
                        }
                    })
                    .collect();
                results.extend(futures::future::join_all(futures).await);
            }
        }

        // Run mutating tools sequentially
        for tu in &mutating {
            results.push(self.execute_one_tool(tu, cancel).await);
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
        cancel: &CancellationToken,
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
            stop_hook_active: None,
            last_assistant_message: None,
        };
        let hook_result = self.hooks.run("PreToolUse", &hook_input, cancel).await;
        if hook_result.blocked {
            let msg = hook_result
                .block_message
                .unwrap_or_else(|| "blocked by hook".into());
            return Err(tool_result_error(&tu.id, format!("Blocked by hook: {msg}")));
        }

        // --- Permission check ---
        if !self.options.bypass_permissions {
            let result = self.permissions.check(tool_name, input);
            match result.behavior {
                PermissionBehavior::Deny => {
                    fire_permission_denied(
                        &self.hooks,
                        &self.session.id,
                        tu,
                        "policy-deny",
                        cancel,
                    )
                    .await;
                    return Err(tool_result_error(
                        &tu.id,
                        format!("Permission denied for tool '{tool_name}'"),
                    ));
                }
                PermissionBehavior::Ask => {
                    // PermissionRequest hook runs before the interactive prompter.
                    // If any hook returns `decision: "block"`, we deny without
                    // bothering the user — matching TS executePermissionRequestHooks.
                    let req_result = fire_permission_request(
                        &self.hooks,
                        &self.session.id,
                        tu,
                        cancel,
                    )
                    .await;
                    if req_result.blocked {
                        fire_permission_denied(
                            &self.hooks,
                            &self.session.id,
                            tu,
                            "permission-request-hook",
                            cancel,
                        )
                        .await;
                        let msg = req_result
                            .block_message
                            .unwrap_or_else(|| "permission denied by hook".into());
                        return Err(tool_result_error(
                            &tu.id,
                            format!("Permission denied for tool '{tool_name}': {msg}"),
                        ));
                    }
                    let decision = self
                        .prompter
                        .prompt(tool_name, input, cancel)
                        .await
                        .unwrap_or(PromptDecision::Deny);
                    match decision {
                        PromptDecision::Deny => {
                            fire_permission_denied(
                                &self.hooks,
                                &self.session.id,
                                tu,
                                "user-deny",
                                cancel,
                            )
                            .await;
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

    async fn execute_one_tool(&mut self, tu: &ToolUseBlock, cancel: &CancellationToken) -> ToolResultBlock {
        debug!("tool_use: {}", tu.name);

        let tool = match self.check_tool_permissions(tu, cancel).await {
            Ok(t) => t,
            Err(result) => return result,
        };

        // Emit ToolStart
        self.emit(AppEvent::ToolStart {
            name: tu.name.clone(),
            input: tu.input.clone(),
        }).await;

        let child_cancel = cancel.child_token();
        let result: ToolResult = match tool.execute(tu.input.clone(), &child_cancel).await {
            Ok(r) => r,
            Err(e) => ToolResult::error(format!("Tool execution error: {e}")),
        };

        let tool_result_block = ToolResultBlock {
            tool_use_id: tu.id.clone(),
            content: Some(Value::String(result.content.clone())),
            is_error: if result.is_error { Some(true) } else { None },
        };

        // Emit ToolEnd
        self.emit(AppEvent::ToolEnd {
            name: tu.name.clone(),
            result,
        }).await;

        // PostToolUse (success) or PostToolUseFailure (error) — never both.
        run_post_tool_hook(&self.hooks, &self.session.id, tu, &tool_result_block, cancel).await;

        tool_result_block
    }
}

/// Fire `PermissionRequest` hook before calling the interactive prompter.
/// Hooks may return `decision: "block"` to deny without prompting; this
/// function just surfaces the HookRunResult so the caller can branch.
async fn fire_permission_request(
    hooks: &HookRunner,
    session_id: &str,
    tu: &ToolUseBlock,
    cancel: &CancellationToken,
) -> cc_hooks::HookRunResult {
    let input = HookInput {
        session_id: session_id.to_string(),
        transcript_path: None,
        cwd: std::env::current_dir()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string(),
        permission_mode: None,
        hook_event_name: "PermissionRequest".into(),
        tool_name: Some(tu.name.clone()),
        tool_input: Some(tu.input.clone()),
        tool_use_id: Some(tu.id.clone()),
        tool_response: None,
        source: None,
        model: None,
        message: None,
        agent_id: None,
        stop_hook_active: None,
        last_assistant_message: None,
    };
    hooks.run("PermissionRequest", &input, cancel).await
}

/// Fire `PermissionDenied` hook after any denial path. Observation-style —
/// result is ignored because the denial already happened. `reason_tag` is
/// passed through `message` so hooks can distinguish policy-deny vs user-deny
/// vs permission-request-hook-deny.
async fn fire_permission_denied(
    hooks: &HookRunner,
    session_id: &str,
    tu: &ToolUseBlock,
    reason_tag: &str,
    cancel: &CancellationToken,
) {
    let input = HookInput {
        session_id: session_id.to_string(),
        transcript_path: None,
        cwd: std::env::current_dir()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string(),
        permission_mode: None,
        hook_event_name: "PermissionDenied".into(),
        tool_name: Some(tu.name.clone()),
        tool_input: Some(tu.input.clone()),
        tool_use_id: Some(tu.id.clone()),
        tool_response: None,
        source: None,
        model: None,
        message: Some(reason_tag.to_string()),
        agent_id: None,
        stop_hook_active: None,
        last_assistant_message: None,
    };
    let _ = hooks.run("PermissionDenied", &input, cancel).await;
}

/// Run PostToolUse (success) or PostToolUseFailure (error) hook after tool execution.
/// Matches TS behavior in src/services/tools/toolExecution.ts — the two events
/// are mutually exclusive: failure path fires `PostToolUseFailure` instead.
async fn run_post_tool_hook(
    hooks: &HookRunner,
    session_id: &str,
    tu: &ToolUseBlock,
    result: &ToolResultBlock,
    cancel: &CancellationToken,
) {
    let event = if result.is_error == Some(true) {
        "PostToolUseFailure"
    } else {
        "PostToolUse"
    };
    let hook_input = HookInput {
        session_id: session_id.to_string(),
        transcript_path: None,
        cwd: std::env::current_dir()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string(),
        permission_mode: None,
        hook_event_name: event.into(),
        tool_name: Some(tu.name.clone()),
        tool_input: Some(tu.input.clone()),
        tool_use_id: Some(tu.id.clone()),
        tool_response: result.content.clone(),
        source: None,
        model: None,
        message: None,
        agent_id: None,
        stop_hook_active: None,
        last_assistant_message: None,
    };
    let _ = hooks.run(event, &hook_input, cancel).await;
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
/// Also strips image blocks from kept messages (§4 contract: images stripped before API call).
pub fn compact_messages(messages: &mut Vec<MessageParam>) {
    const KEEP_RECENT: usize = 20;
    if messages.len() <= KEEP_RECENT + 1 {
        return;
    }

    debug!("compacting {} messages → keeping last {KEEP_RECENT}", messages.len());

    // Keep first message (initial user message) + last KEEP_RECENT
    let first = messages[0].clone();
    let keep_from = messages.len().saturating_sub(KEEP_RECENT);
    let recent: Vec<MessageParam> = messages[keep_from..].iter().map(strip_images).collect();

    messages.clear();
    messages.push(first);

    // Add a system-style summary notice as a user message
    messages.push(MessageParam::user(
        "[Context compacted: earlier messages trimmed to stay within token limits]",
    ));

    messages.extend(recent);
}

/// Strip image blocks from a message to reduce token count after compaction.
/// Removes `ContentBlock::Image` blocks and image entries from ToolResult content arrays.
fn strip_images(msg: &MessageParam) -> MessageParam {
    let content = match &msg.content {
        MessageContent::Text(_) => return msg.clone(),
        MessageContent::Blocks(blocks) => {
            let filtered: Vec<ContentBlock> = blocks
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Image(_) => None,
                    ContentBlock::ToolResult(tr) => {
                        // Strip image blocks from tool_result content arrays
                        let content = tr.content.as_ref().map(strip_images_from_value);
                        Some(ContentBlock::ToolResult(cc_core::ToolResultBlock {
                            tool_use_id: tr.tool_use_id.clone(),
                            content,
                            is_error: tr.is_error,
                        }))
                    }
                    other => Some(other.clone()),
                })
                .collect();
            MessageContent::Blocks(filtered)
        }
    };
    MessageParam { role: msg.role, content }
}

/// Remove image-type blocks from a tool_result content value.
fn strip_images_from_value(v: &serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Array(arr) => {
            let filtered: Vec<serde_json::Value> = arr
                .iter()
                .filter(|item| {
                    // Remove blocks with type "image"
                    item.get("type").and_then(|t| t.as_str()) != Some("image")
                })
                .cloned()
                .collect();
            serde_json::Value::Array(filtered)
        }
        other => other.clone(),
    }
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
        let mut msgs: Vec<MessageParam> = (0..50)
            .map(|i| {
                let role = if i % 2 == 0 { Role::User } else { Role::Assistant };
                msg(role, &format!("m{i}"))
            })
            .collect();
        compact_messages(&mut msgs);
        assert_eq!(msgs.len(), 22, "first + boundary + last 20 = 22");
        if let MessageContent::Text(t) = &msgs[0].content {
            assert_eq!(t, "m0");
        } else {
            panic!("expected text content for first message");
        }
        if let MessageContent::Text(t) = &msgs[1].content {
            assert!(t.contains("Context compacted"), "marker text present");
        } else {
            panic!("expected text content for marker");
        }
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

    #[tokio::test]
    async fn permission_request_hook_fires() {
        use cc_core::hook::HooksSettings;
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("req.txt");
        let settings: HooksSettings = serde_json::from_str(&format!(
            r#"{{"PermissionRequest": [{{"hooks": [{{"type": "command", "command": "touch {}"}}]}}]}}"#,
            marker.display()
        ))
        .unwrap();
        let hooks = HookRunner::new(&settings, reqwest::Client::new());
        let tu = ToolUseBlock {
            id: "tu_1".into(),
            name: "Bash".into(),
            input: serde_json::json!({"command": "rm -rf /"}),
        };
        let cancel = CancellationToken::new();
        let result = fire_permission_request(&hooks, "sess-1", &tu, &cancel).await;
        assert!(!result.blocked);
        assert!(marker.exists(), "PermissionRequest hook should fire");
    }

    #[tokio::test]
    async fn permission_request_hook_can_block() {
        // A PermissionRequest hook returning exit 2 should set `blocked=true`,
        // letting the engine deny the tool without calling the interactive prompter.
        use cc_core::hook::HooksSettings;
        let settings: HooksSettings = serde_json::from_str(
            r#"{"PermissionRequest": [{"hooks": [{"type": "command", "command": "printf 'nope' && exit 2"}]}]}"#,
        )
        .unwrap();
        let hooks = HookRunner::new(&settings, reqwest::Client::new());
        let tu = ToolUseBlock {
            id: "tu_1".into(),
            name: "Bash".into(),
            input: serde_json::json!({}),
        };
        let cancel = CancellationToken::new();
        let result = fire_permission_request(&hooks, "sess-1", &tu, &cancel).await;
        assert!(result.blocked);
        assert_eq!(result.block_message.as_deref(), Some("nope"));
    }

    #[tokio::test]
    async fn permission_denied_hook_fires() {
        use cc_core::hook::HooksSettings;
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("denied.txt");
        let settings: HooksSettings = serde_json::from_str(&format!(
            r#"{{"PermissionDenied": [{{"hooks": [{{"type": "command", "command": "cat > {} <<< \"$CLAUDE_SESSION_ID\""}}]}}]}}"#,
            marker.display()
        ))
        .unwrap();
        let hooks = HookRunner::new(&settings, reqwest::Client::new());
        let tu = ToolUseBlock {
            id: "tu_1".into(),
            name: "Bash".into(),
            input: serde_json::json!({}),
        };
        let cancel = CancellationToken::new();
        fire_permission_denied(&hooks, "sess-abc", &tu, "policy-deny", &cancel).await;
        assert!(marker.exists(), "PermissionDenied hook should fire");
        let contents = std::fs::read_to_string(&marker).unwrap();
        assert!(
            contents.trim() == "sess-abc",
            "CLAUDE_SESSION_ID should be injected, got {contents:?}"
        );
    }

    #[tokio::test]
    async fn post_tool_use_hook_fires_on_success() {
        use cc_core::hook::HooksSettings;
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("fired.txt");
        let settings: HooksSettings = serde_json::from_str(&format!(
            r#"{{"PostToolUse": [{{"hooks": [{{"type": "command", "command": "touch {}"}}]}}]}}"#,
            marker.display()
        ))
        .unwrap();
        let hooks = HookRunner::new(&settings, reqwest::Client::new());
        let tu = ToolUseBlock {
            id: "tu_1".into(),
            name: "Bash".into(),
            input: serde_json::json!({"command": "echo ok"}),
        };
        let result = ToolResultBlock {
            tool_use_id: "tu_1".into(),
            content: Some(serde_json::Value::String("ok".into())),
            is_error: None,
        };
        let cancel = CancellationToken::new();
        run_post_tool_hook(&hooks, "sess-1", &tu, &result, &cancel).await;
        assert!(marker.exists(), "PostToolUse hook should have fired");
    }

    #[tokio::test]
    async fn post_tool_use_failure_hook_fires_on_error() {
        use cc_core::hook::HooksSettings;
        let dir = tempfile::tempdir().unwrap();
        let success_marker = dir.path().join("success.txt");
        let failure_marker = dir.path().join("failure.txt");
        let settings: HooksSettings = serde_json::from_str(&format!(
            r#"{{
                "PostToolUse": [{{"hooks": [{{"type": "command", "command": "touch {}"}}]}}],
                "PostToolUseFailure": [{{"hooks": [{{"type": "command", "command": "touch {}"}}]}}]
            }}"#,
            success_marker.display(),
            failure_marker.display()
        ))
        .unwrap();
        let hooks = HookRunner::new(&settings, reqwest::Client::new());
        let tu = ToolUseBlock {
            id: "tu_1".into(),
            name: "Bash".into(),
            input: serde_json::json!({"command": "false"}),
        };
        let result = ToolResultBlock {
            tool_use_id: "tu_1".into(),
            content: Some(serde_json::Value::String("boom".into())),
            is_error: Some(true),
        };
        let cancel = CancellationToken::new();
        run_post_tool_hook(&hooks, "sess-1", &tu, &result, &cancel).await;
        assert!(
            failure_marker.exists(),
            "PostToolUseFailure hook should have fired"
        );
        assert!(
            !success_marker.exists(),
            "PostToolUse must not fire on error path"
        );
    }

    #[test]
    fn compact_messages_strips_images_from_kept_messages() {
        use cc_core::{ContentBlock, ImageBlock, ImageSource, MessageContent, ToolResultBlock};
        use serde_json::json;

        let image_block = ContentBlock::Image(ImageBlock {
            source: ImageSource::Base64 {
                media_type: "image/png".into(),
                data: "abc123".into(),
            },
        });
        let text_block = ContentBlock::text("hello");

        // Build enough messages for compaction (> 21)
        let mut msgs: Vec<MessageParam> = (0..20)
            .map(|i| msg(if i % 2 == 0 { Role::User } else { Role::Assistant }, &format!("m{i}")))
            .collect();

        // Add a message with image + text blocks as the last one
        msgs.push(MessageParam {
            role: Role::User,
            content: MessageContent::Blocks(vec![image_block.clone(), text_block.clone()]),
        });

        // Add a tool_result message with image content
        msgs.push(MessageParam {
            role: Role::User,
            content: MessageContent::Blocks(vec![ContentBlock::ToolResult(ToolResultBlock {
                tool_use_id: "tu1".into(),
                content: Some(json!([
                    {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "xyz"}},
                    {"type": "text", "text": "description"}
                ])),
                is_error: None,
            })]),
        });

        compact_messages(&mut msgs);

        // Verify no image blocks remain in any kept message
        for m in &msgs {
            if let MessageContent::Blocks(blocks) = &m.content {
                for b in blocks {
                    assert!(!matches!(b, ContentBlock::Image(_)), "image block should be stripped");
                    if let ContentBlock::ToolResult(tr) = b {
                        if let Some(serde_json::Value::Array(items)) = &tr.content {
                            for item in items {
                                assert_ne!(
                                    item.get("type").and_then(|t| t.as_str()),
                                    Some("image"),
                                    "image in tool_result should be stripped"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}
