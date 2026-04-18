use cc_api::{
    ApiClient, ContentBlockDelta, ContentBlockStartData, CreateMessageRequest, StreamAccumulator,
    StreamEvent, UsageTracker,
};
use cc_core::{
    AppEvent, CcError, CcResult, ContentBlock, Message, MessageContent, MessageParam,
    PermissionBehavior, PermissionPrompter, PromptDecision, Role, StopReason, SystemBlock,
    ToolResultBlock, ToolUseBlock, Usage,
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

/// Construction bundle for `QueryEngine::new`.
///
/// Collected into a struct so callers pass named fields instead of
/// matching an 8-arg positional signature (easy to swap two Arc<_>
/// parameters undetected). Use struct-init syntax at the call site.
pub struct QueryEngineConfig {
    pub api: ApiClient,
    pub tools: Arc<ToolRegistry>,
    pub permissions: PermissionEngine,
    pub hooks: Arc<HookRunner>,
    pub session: Session,
    pub system_blocks: Vec<SystemBlock>,
    pub options: QueryOptions,
    pub prompter: Arc<dyn PermissionPrompter>,
}

impl QueryEngine {
    pub fn new(cfg: QueryEngineConfig) -> Self {
        let QueryEngineConfig {
            api,
            tools,
            permissions,
            hooks,
            session,
            system_blocks,
            options,
            prompter,
        } = cfg;
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
                return Err(CcError::Other(format!("exceeded max turns ({MAX_TURNS})")));
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

            // 4. Stream response.
            //
            // We consume the raw `StreamEvent` receiver directly (not
            // `ApiClient::complete_message`) so we can forward user-visible
            // deltas to `events_tx` with `.send().await`. Using async send
            // propagates TUI backpressure upstream instead of silently
            // dropping events under load (C3 regression guard in
            // `openspec/AUDIT-phase3.md`).
            let mut text_buf = String::new();
            let mut tool_use_blocks: Vec<ToolUseBlock> = Vec::new();

            let rx = self.api.stream_message(req, cancel).await?;
            let (message, usage) = drain_stream(
                rx,
                &mut on_text,
                &mut text_buf,
                self.events_tx.as_ref(),
                cancel,
            )
            .await?;

            // §4 contract: if streaming was interrupted (cancel fired), save partial
            // text with interrupt marker before propagating cancellation.
            if cancel.is_cancelled() && !text_buf.is_empty() {
                let mut interrupted_content = message.content.clone();
                interrupted_content.push(ContentBlock::text("\n[Interrupted by user]"));
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
                    let tool_results = self.execute_tools(&tool_use_blocks, cancel).await?;

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
                    let stop_input = HookInput::base(self.session.id.clone(), "Stop")
                        .with_transcript_path(self.session.transcript_path().to_string_lossy())
                        .with_model(self.options.model.clone())
                        .with_stop_hook_active(true)
                        .with_last_assistant_message(if final_text.is_empty() {
                            None
                        } else {
                            Some(final_text.clone())
                        });
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
        let (read_only, mutating): (Vec<_>, Vec<_>) = tool_use_blocks
            .iter()
            .partition(|tu| self.tools.get(&tu.name).is_some_and(|t| t.is_read_only()));

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
                                let _ = tx
                                    .send(AppEvent::ToolStart {
                                        name: tu.name.clone(),
                                        input: tu.input.clone(),
                                    })
                                    .await;
                            }

                            let result: ToolResult = match tool
                                .execute(tu.input.clone(), &cancel)
                                .await
                            {
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
                                let _ = tx
                                    .send(AppEvent::ToolEnd {
                                        name: tu.name.clone(),
                                        result: result.clone(),
                                    })
                                    .await;
                            }

                            // PostToolUse hook
                            run_post_tool_hook(
                                &hooks,
                                &session_id,
                                &tu,
                                &tool_result_block,
                                &cancel,
                            )
                            .await;

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
        let hook_input = HookInput::base(self.session.id.clone(), "PreToolUse").with_tool(tu);
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
                    let req_result =
                        fire_permission_request(&self.hooks, &self.session.id, tu, cancel).await;
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

    async fn execute_one_tool(
        &mut self,
        tu: &ToolUseBlock,
        cancel: &CancellationToken,
    ) -> ToolResultBlock {
        debug!("tool_use: {}", tu.name);

        let tool = match self.check_tool_permissions(tu, cancel).await {
            Ok(t) => t,
            Err(result) => return result,
        };

        // Emit ToolStart
        self.emit(AppEvent::ToolStart {
            name: tu.name.clone(),
            input: tu.input.clone(),
        })
        .await;

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
        })
        .await;

        // PostToolUse (success) or PostToolUseFailure (error) — never both.
        run_post_tool_hook(
            &self.hooks,
            &self.session.id,
            tu,
            &tool_result_block,
            cancel,
        )
        .await;

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
    let input = HookInput::base(session_id, "PermissionRequest").with_tool(tu);
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
    let input = HookInput::base(session_id, "PermissionDenied")
        .with_tool(tu)
        .with_message(reason_tag);
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
    let hook_input = HookInput::base(session_id, event)
        .with_tool(tu)
        .with_tool_response(result.content.clone());
    let _ = hooks.run(event, &hook_input, cancel).await;
}

fn tool_result_error(tool_use_id: &str, message: impl Into<String>) -> ToolResultBlock {
    ToolResultBlock {
        tool_use_id: tool_use_id.to_string(),
        content: Some(Value::String(message.into())),
        is_error: Some(true),
    }
}

/// Drive a streaming response to completion, forwarding user-visible events
/// to the TUI with async backpressure (`send().await`) rather than the
/// previous lossy `try_send` path.
///
/// See `openspec/changes/fix-tui-event-dropping/` (C3): under TUI load the
/// old `try_send` silently dropped `StreamDelta`/`StreamThinking`/
/// `StreamToolUse` events, breaking the M3 "token-by-token" AC. Using
/// `send().await` here makes the engine block until the TUI drains, which
/// is the correct behaviour for a streaming UI.
///
/// If the TUI receiver is dropped mid-turn we call `cancel.cancel()` so the
/// upstream HTTP stream is torn down quickly and the caller observes
/// `CcError::Cancelled` at the §4 partial-save site.
pub(crate) async fn drain_stream<F>(
    mut rx: mpsc::Receiver<CcResult<StreamEvent>>,
    on_text: &mut F,
    text_buf: &mut String,
    events_tx: Option<&mpsc::Sender<AppEvent>>,
    cancel: &CancellationToken,
) -> CcResult<(Message, Usage)>
where
    F: FnMut(&str),
{
    let mut acc = StreamAccumulator::default();

    while let Some(item) = rx.recv().await {
        let event = item?;
        match &event {
            StreamEvent::ContentBlockDelta {
                delta: ContentBlockDelta::TextDelta { text },
                ..
            } => {
                text_buf.push_str(text);
                on_text(text);
                if let Some(tx) = events_tx {
                    if tx.send(AppEvent::StreamDelta(text.clone())).await.is_err() {
                        cancel.cancel();
                    }
                }
            }
            StreamEvent::ContentBlockDelta {
                delta: ContentBlockDelta::ThinkingDelta { thinking },
                ..
            } => {
                if let Some(tx) = events_tx {
                    if tx
                        .send(AppEvent::StreamThinking(thinking.clone()))
                        .await
                        .is_err()
                    {
                        cancel.cancel();
                    }
                }
            }
            StreamEvent::ContentBlockStart {
                content_block: ContentBlockStartData::ToolUse { id, name, input },
                ..
            } => {
                if let Some(tx) = events_tx {
                    let tu = ToolUseBlock {
                        id: id.clone(),
                        name: name.clone(),
                        input: input.clone(),
                    };
                    if tx.send(AppEvent::StreamToolUse(tu)).await.is_err() {
                        cancel.cancel();
                    }
                }
            }
            _ => {}
        }
        acc.apply(&event);
    }

    acc.into_message_and_usage()
        .map_err(|e| CcError::api(format!("stream accumulator: {e}")))
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

    debug!(
        "compacting {} messages → keeping last {KEEP_RECENT}",
        messages.len()
    );

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
    MessageParam {
        role: msg.role,
        content,
    }
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
                let role = if i % 2 == 0 {
                    Role::User
                } else {
                    Role::Assistant
                };
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
        let mut msgs: Vec<MessageParam> =
            (0..10).map(|i| msg(Role::User, &format!("m{i}"))).collect();
        let before = msgs.clone();
        compact_messages(&mut msgs);
        assert_eq!(
            msgs.len(),
            before.len(),
            "no compaction when ≤ KEEP_RECENT + 1"
        );
    }

    #[tokio::test]
    async fn permission_request_hook_fires() {
        use cc_core::hook::HooksSettings;
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("req.txt");
        let settings: HooksSettings = serde_json::from_str(&format!(
            r#"{{"PermissionRequest": [{{"hooks": [{{"type": "command", "command": "touch {}", "unsafe_shell": true}}]}}]}}"#,
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
            r#"{"PermissionRequest": [{"hooks": [{"type": "command", "command": "printf 'nope' && exit 2", "unsafe_shell": true}]}]}"#,
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
            r#"{{"PermissionDenied": [{{"hooks": [{{"type": "command", "command": "cat > {} <<< \"$CLAUDE_SESSION_ID\"", "unsafe_shell": true}}]}}]}}"#,
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
            r#"{{"PostToolUse": [{{"hooks": [{{"type": "command", "command": "touch {}", "unsafe_shell": true}}]}}]}}"#,
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
                "PostToolUse": [{{"hooks": [{{"type": "command", "command": "touch {}", "unsafe_shell": true}}]}}],
                "PostToolUseFailure": [{{"hooks": [{{"type": "command", "command": "touch {}", "unsafe_shell": true}}]}}]
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
            .map(|i| {
                msg(
                    if i % 2 == 0 {
                        Role::User
                    } else {
                        Role::Assistant
                    },
                    &format!("m{i}"),
                )
            })
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
                    assert!(
                        !matches!(b, ContentBlock::Image(_)),
                        "image block should be stripped"
                    );
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

    // ---- drain_stream regression tests (C3 guard) ----
    //
    // These exercise the engine's per-turn stream consumer directly so we
    // don't need a mock HTTP server. The invariant under test is: when a
    // slow TUI receiver backpressures the forwarder, the engine must block
    // on `send().await` rather than silently drop events. Dropping was the
    // Phase-3 C3 regression (`openspec/changes/fix-tui-event-dropping/`).

    use cc_api::{ContentBlockStartData, MessageStartData, StreamEvent};

    fn text_delta_event(s: &str) -> StreamEvent {
        StreamEvent::ContentBlockDelta {
            index: 0,
            delta: cc_api::ContentBlockDelta::TextDelta { text: s.into() },
        }
    }

    fn message_start_event(id: &str) -> StreamEvent {
        StreamEvent::MessageStart {
            message: MessageStartData {
                id: id.into(),
                kind: "message".into(),
                role: "assistant".into(),
                content: vec![],
                model: "claude-test".into(),
                stop_reason: None,
                stop_sequence: None,
                usage: cc_core::Usage {
                    input_tokens: 1,
                    output_tokens: 0,
                    cache_creation_input_tokens: None,
                    cache_read_input_tokens: None,
                },
            },
        }
    }

    fn content_block_start_text(index: u32) -> StreamEvent {
        StreamEvent::ContentBlockStart {
            index,
            content_block: ContentBlockStartData::Text {
                text: String::new(),
            },
        }
    }

    /// Slow consumer + fast producer: every inbound delta must land in the
    /// TUI channel, in order, with no drops.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drain_stream_no_drops_under_backpressure_1000_deltas() {
        let (in_tx, in_rx) = mpsc::channel::<CcResult<StreamEvent>>(4);
        let (out_tx, mut out_rx) = mpsc::channel::<AppEvent>(4);
        let cancel = CancellationToken::new();

        // Producer: push 1000 text deltas + the framing events an
        // accumulator expects. We send them as fast as possible; the
        // channel is tiny (4) so the forwarder must backpressure.
        let producer = tokio::spawn(async move {
            in_tx.send(Ok(message_start_event("msg_1"))).await.unwrap();
            in_tx.send(Ok(content_block_start_text(0))).await.unwrap();
            for i in 0..1000 {
                in_tx
                    .send(Ok(text_delta_event(&format!("{i}"))))
                    .await
                    .unwrap();
            }
            drop(in_tx);
        });

        // Drain: run the real engine helper. Slow the consumer so the
        // forwarder is definitely blocked on send().await at times.
        let drain = tokio::spawn(async move {
            let mut on_text = |_: &str| {};
            let mut text_buf = String::new();
            drain_stream(in_rx, &mut on_text, &mut text_buf, Some(&out_tx), &cancel).await
        });

        let mut received: Vec<String> = Vec::with_capacity(1000);
        while let Some(ev) = out_rx.recv().await {
            match ev {
                AppEvent::StreamDelta(s) => {
                    received.push(s);
                    tokio::time::sleep(std::time::Duration::from_micros(200)).await;
                }
                other => panic!("unexpected event: {other:?}"),
            }
        }

        producer.await.unwrap();
        let (message, _usage) = drain.await.unwrap().expect("drain ok");
        assert_eq!(received.len(), 1000, "no drops under backpressure");
        for (i, s) in received.iter().enumerate() {
            assert_eq!(s, &format!("{i}"), "out-of-order at index {i}");
        }
        // Accumulator should have assembled the full text block.
        let total: String = (0..1000).map(|i| i.to_string()).collect();
        assert!(
            message
                .content
                .iter()
                .any(|b| matches!(b, ContentBlock::Text(t) if t.text == total)),
            "accumulator text block should match producer"
        );
    }

    /// If the TUI receiver is dropped mid-turn, drain_stream cancels the
    /// CancellationToken and returns Ok with whatever accumulated so far.
    #[tokio::test]
    async fn drain_stream_cancels_when_tui_receiver_drops() {
        let (in_tx, in_rx) = mpsc::channel::<CcResult<StreamEvent>>(8);
        let (out_tx, out_rx) = mpsc::channel::<AppEvent>(1);
        let cancel = CancellationToken::new();

        drop(out_rx);

        let producer = tokio::spawn(async move {
            in_tx.send(Ok(message_start_event("msg_2"))).await.unwrap();
            in_tx.send(Ok(content_block_start_text(0))).await.unwrap();
            for i in 0..50 {
                if in_tx
                    .send(Ok(text_delta_event(&format!("{i}"))))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            drop(in_tx);
        });

        let mut on_text = |_: &str| {};
        let mut text_buf = String::new();
        let result = drain_stream(in_rx, &mut on_text, &mut text_buf, Some(&out_tx), &cancel).await;

        producer.await.unwrap();
        // drain_stream itself does not error on receiver drop — it cancels
        // the upstream token and keeps accumulating the remaining frames
        // that were already in the mpsc buffer. Callers observe
        // cancellation via `cancel.is_cancelled()`.
        assert!(result.is_ok(), "drain_stream should not error");
        assert!(
            cancel.is_cancelled(),
            "cancel token must fire when TUI drops"
        );
    }
}
