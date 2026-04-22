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
            let (message, usage, bad_tool_inputs) = drain_stream(
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
                    // Split tool_use blocks into those with valid JSON input
                    // (execute normally) and those whose streamed JSON
                    // arguments failed to parse (synthesize an `is_error:
                    // true` tool_result so the model can retry). This is
                    // the engine-side piece of H1 — openspec
                    // `fix-api-stream-toolinput-fallback` §2.1 / §2.2.
                    // The placeholder ToolUse blocks with `input = {}` are
                    // already in the assistant message (see
                    // `StreamAccumulator::into_content_recovering`) so id
                    // pairing is preserved.
                    use std::collections::HashSet;
                    let bad_ids: HashSet<&str> = bad_tool_inputs
                        .iter()
                        .map(|(id, _, _)| id.as_str())
                        .collect();
                    let valid_blocks: Vec<ToolUseBlock> = tool_use_blocks
                        .iter()
                        .filter(|b| !bad_ids.contains(b.id.as_str()))
                        .cloned()
                        .collect();
                    let valid_results = if valid_blocks.is_empty() {
                        Vec::new()
                    } else {
                        self.execute_tools(&valid_blocks, cancel).await?
                    };
                    let tool_results =
                        merge_tool_results(&tool_use_blocks, valid_results, &bad_tool_inputs);

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
/// Consume the streaming `rx` into a completed `(Message, Usage, bad_tool_inputs)`
/// tuple, forwarding user-visible deltas onto `events_tx` along the way.
///
/// The third component carries `(id, name, raw_fragment)` for every
/// `tool_use` block whose JSON buffer failed to parse at end-of-stream.
/// `run_turn` pairs each of these with a synthetic `is_error: true`
/// `tool_result` block so the model can retry with well-formed
/// arguments (openspec `fix-api-stream-toolinput-fallback` §2).
/// The `Message.content` still carries the placeholder `ToolUseBlock`
/// with `input = {}` so the id-pairing invariant is preserved.
pub(crate) async fn drain_stream<F>(
    mut rx: mpsc::Receiver<CcResult<StreamEvent>>,
    on_text: &mut F,
    text_buf: &mut String,
    events_tx: Option<&mpsc::Sender<AppEvent>>,
    cancel: &CancellationToken,
) -> CcResult<(Message, Usage, Vec<(String, String, String)>)>
where
    F: FnMut(&str),
{
    let mut acc = StreamAccumulator::default();

    while let Some(item) = rx.recv().await {
        let event = item?;
        // Forward user-visible deltas to the TUI. If the TUI receiver
        // has been dropped, stop consuming upstream immediately:
        // cancel the shared token (so the HTTP stream is torn down)
        // and break out of this loop. Continuing to drain buffered
        // upstream events after the receiver is gone is what the
        // 2026-04-21 QA reopen of `fix-tui-event-dropping` §1.2 / §3.2
        // called out — it is not what the "treat SendError as
        // cancellation" contract says. Returning here still produces
        // a well-formed `Message` via the accumulator so the partial-
        // save path in `run_turn` can persist whatever landed first.
        let forward = match &event {
            StreamEvent::ContentBlockDelta {
                delta: ContentBlockDelta::TextDelta { text },
                ..
            } => {
                text_buf.push_str(text);
                on_text(text);
                events_tx.map(|tx| tx.send(AppEvent::StreamDelta(text.clone())))
            }
            StreamEvent::ContentBlockDelta {
                delta: ContentBlockDelta::ThinkingDelta { thinking },
                ..
            } => events_tx.map(|tx| tx.send(AppEvent::StreamThinking(thinking.clone()))),
            StreamEvent::ContentBlockStart {
                content_block: ContentBlockStartData::ToolUse { id, name, input },
                ..
            } => {
                let tu = ToolUseBlock {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                };
                events_tx.map(|tx| tx.send(AppEvent::StreamToolUse(tu)))
            }
            _ => None,
        };
        acc.apply(&event);
        if let Some(fut) = forward {
            if fut.await.is_err() {
                cancel.cancel();
                break;
            }
        }
    }

    Ok(acc.into_message_and_usage_recovering())
}

/// Build the synthetic `tool_result` block a model sees when one of its
/// `tool_use` blocks failed to deliver valid JSON arguments. Carries a
/// clear retry instruction + the raw fragment (already truncated to
/// `TOOL_INPUT_RAW_MAX` by the accumulator) so the model has enough
/// context to self-correct. Shared by `merge_tool_results` and the
/// integration-level tests.
pub(crate) fn synthesize_bad_tool_input_result(id: &str, name: &str, raw: &str) -> ToolResultBlock {
    ToolResultBlock {
        tool_use_id: id.to_string(),
        content: Some(Value::String(format!(
            "tool input for '{name}' was not valid JSON and was discarded. \
             Retry this tool call with a well-formed JSON argument object. \
             Raw fragment (truncated): {raw}"
        ))),
        is_error: Some(true),
    }
}

/// Merge `valid_results` (the output of `execute_tools`) with synthetic
/// error results for every `bad_tool_inputs` entry, then reorder to
/// match the original `tool_use_blocks` sequence. The Anthropic API
/// requires 1:1 ordered pairing between `tool_use` and `tool_result`
/// blocks within a turn; callers MUST feed the returned Vec directly
/// into a single user-message without further reordering.
///
/// openspec `fix-api-stream-toolinput-fallback` §2.1 / §2.2.
pub(crate) fn merge_tool_results(
    tool_use_blocks: &[ToolUseBlock],
    valid_results: Vec<ToolResultBlock>,
    bad_tool_inputs: &[(String, String, String)],
) -> Vec<ToolResultBlock> {
    use std::collections::HashMap;
    let mut by_id: HashMap<String, ToolResultBlock> = valid_results
        .into_iter()
        .map(|r| (r.tool_use_id.clone(), r))
        .collect();
    for (id, name, raw) in bad_tool_inputs {
        by_id.insert(id.clone(), synthesize_bad_tool_input_result(id, name, raw));
    }
    tool_use_blocks
        .iter()
        .filter_map(|tu| by_id.remove(&tu.id))
        .collect()
}

/// Auto-compact threshold (§4.5).
/// = effective_context_window - 13,000
/// Default effective_context_window = 200,000 (claude-sonnet-4-6).
fn compact_threshold() -> u32 {
    let context_window: u32 = std::env::var("CLAUDE_CODE_MAX_CONTEXT_TOKENS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(cc_core::model::models::DEFAULT_CONTEXT_WINDOW);
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
        let (message, _usage, bad) = drain.await.unwrap().expect("drain ok");
        assert!(
            bad.is_empty(),
            "well-formed stream must not flag any bad tool_use"
        );
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
    /// CancellationToken AND breaks out of the stream loop immediately
    /// rather than continuing to drain already-buffered upstream events.
    ///
    /// Proof: count how many events the producer manages to send. With the
    /// break-on-SendError fix, drain_stream returns after the first forward
    /// fails, drops `in_rx`, and the producer's subsequent `send().await`
    /// calls fail — so the producer never gets all 50 deltas through. The
    /// pre-fix behaviour ("cancel but keep consuming") let the producer
    /// push every event into the upstream channel, so this discriminator
    /// catches the 2026-04-21 QA regression directly.
    #[tokio::test]
    async fn drain_stream_cancels_when_tui_receiver_drops() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let (in_tx, in_rx) = mpsc::channel::<CcResult<StreamEvent>>(8);
        let (out_tx, out_rx) = mpsc::channel::<AppEvent>(1);
        let cancel = CancellationToken::new();

        drop(out_rx);

        let sent = Arc::new(AtomicUsize::new(0));
        let sent_clone = sent.clone();
        let producer = tokio::spawn(async move {
            in_tx.send(Ok(message_start_event("msg_2"))).await.unwrap();
            sent_clone.fetch_add(1, Ordering::Relaxed);
            in_tx.send(Ok(content_block_start_text(0))).await.unwrap();
            sent_clone.fetch_add(1, Ordering::Relaxed);
            for i in 0..50 {
                if in_tx
                    .send(Ok(text_delta_event(&format!("{i}"))))
                    .await
                    .is_err()
                {
                    return;
                }
                sent_clone.fetch_add(1, Ordering::Relaxed);
            }
            drop(in_tx);
        });

        let mut on_text = |_: &str| {};
        let mut text_buf = String::new();
        let result = drain_stream(in_rx, &mut on_text, &mut text_buf, Some(&out_tx), &cancel).await;

        producer.await.unwrap();
        assert!(result.is_ok(), "drain_stream should not error");
        assert!(
            cancel.is_cancelled(),
            "cancel token must fire when TUI drops"
        );

        // Strong assertion: drain_stream must tear down the upstream
        // consumer promptly. If it kept draining buffered events (the
        // pre-fix regression the QA flagged), the producer would get
        // all 52 sends through (msg_start + content_block_start + 50
        // deltas). With the break the producer's channel closes
        // before it finishes, so the total stays well below 52.
        let total_sent = sent.load(Ordering::Relaxed);
        assert!(
            total_sent < 52,
            "drain_stream did not break promptly on receiver close: \
             producer managed to send {total_sent}/52 events before in_rx closed — \
             that is the pre-fix 'cancel but keep draining' behaviour."
        );
    }

    // ── openspec fix-api-stream-toolinput-fallback §2 / §3.2 ──────────────
    //
    // These tests cover the engine-side catch that was claimed [x] in
    // tasks.md as of commit 01beab2 but was in fact never ported to
    // `cc-query` — `drain_stream` used to map every `StreamError` to a
    // flat `CcError::api(...)`, which aborted the whole turn instead of
    // letting the model retry with corrective feedback. The fix (this
    // commit) uses `StreamAccumulator::into_message_and_usage_recovering`
    // so a malformed tool_use JSON tail yields:
    //   (a) an assistant message with a placeholder `ToolUseBlock` so
    //       id pairing survives,
    //   (b) a non-empty `bad_tool_inputs` list that `run_turn` converts
    //       into a synthetic `is_error: true` tool_result block.

    fn content_block_start_tool_use(index: u32, id: &str, name: &str) -> StreamEvent {
        StreamEvent::ContentBlockStart {
            index,
            content_block: ContentBlockStartData::ToolUse {
                id: id.to_string(),
                name: name.to_string(),
                input: serde_json::Value::Object(Default::default()),
            },
        }
    }

    fn input_json_delta_event(index: u32, partial: &str) -> StreamEvent {
        StreamEvent::ContentBlockDelta {
            index,
            delta: ContentBlockDelta::InputJsonDelta {
                partial_json: partial.to_string(),
            },
        }
    }

    /// End-of-stream with a half-finished tool_use JSON buffer. The old
    /// code errored out of `drain_stream`. The fixed code returns Ok
    /// with a placeholder `ToolUse` block in the message and the bad
    /// fragment in the third component.
    #[tokio::test]
    async fn drain_stream_recovers_malformed_tool_use_into_bad_list() {
        let (in_tx, in_rx) = mpsc::channel::<CcResult<StreamEvent>>(8);
        let cancel = CancellationToken::new();

        in_tx.send(Ok(message_start_event("msg_x"))).await.unwrap();
        in_tx
            .send(Ok(content_block_start_tool_use(0, "tool_9", "Write")))
            .await
            .unwrap();
        in_tx
            .send(Ok(input_json_delta_event(
                0,
                r#"{"file_path":"/tmp/x","content"#,
            )))
            .await
            .unwrap();
        drop(in_tx);

        let mut on_text = |_: &str| {};
        let mut text_buf = String::new();
        let (message, _usage, bad) =
            drain_stream(in_rx, &mut on_text, &mut text_buf, None, &cancel)
                .await
                .expect("drain_stream must now recover, not error");

        assert_eq!(bad.len(), 1);
        assert_eq!(bad[0].0, "tool_9");
        assert_eq!(bad[0].1, "Write");
        assert!(bad[0].2.contains("/tmp/x"));

        // The assistant message still carries a placeholder tool_use
        // block so the id pairing the API demands is intact.
        let has_placeholder = message.content.iter().any(|b| match b {
            ContentBlock::ToolUse(tu) => {
                tu.id == "tool_9"
                    && tu.name == "Write"
                    && tu.input == serde_json::Value::Object(Default::default())
            }
            _ => false,
        });
        assert!(has_placeholder, "placeholder ToolUse block must survive");
    }

    /// `merge_tool_results` is the pure-function half of the §2 fix. It
    /// must: (a) reorder results to match the original tool_use_blocks
    /// sequence; (b) synthesize an `is_error: true` result for every id
    /// in `bad_tool_inputs`; (c) preserve `valid_results` for ids that
    /// aren't in the bad list.
    #[test]
    fn merge_tool_results_preserves_order_and_synthesises_errors() {
        let tu_good_1 = ToolUseBlock {
            id: "id_good_1".into(),
            name: "Bash".into(),
            input: serde_json::json!({"command":"echo hi"}),
        };
        let tu_bad = ToolUseBlock {
            id: "id_bad".into(),
            name: "Edit".into(),
            input: serde_json::Value::Object(Default::default()),
        };
        let tu_good_2 = ToolUseBlock {
            id: "id_good_2".into(),
            name: "Read".into(),
            input: serde_json::json!({"file_path":"/x"}),
        };
        let original = vec![tu_good_1.clone(), tu_bad.clone(), tu_good_2.clone()];

        let valid_results = vec![
            ToolResultBlock {
                tool_use_id: "id_good_2".into(),
                content: Some(serde_json::Value::String("read ok".into())),
                is_error: Some(false),
            },
            ToolResultBlock {
                tool_use_id: "id_good_1".into(),
                content: Some(serde_json::Value::String("hi".into())),
                is_error: Some(false),
            },
        ];
        let bad = vec![(
            "id_bad".to_string(),
            "Edit".to_string(),
            r#"{"file_path":"/a","old_stri"#.to_string(),
        )];

        let merged = merge_tool_results(&original, valid_results, &bad);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[0].tool_use_id, "id_good_1");
        assert_eq!(merged[0].is_error, Some(false));
        assert_eq!(merged[1].tool_use_id, "id_bad");
        assert_eq!(
            merged[1].is_error,
            Some(true),
            "bad id must be flagged is_error:true"
        );
        let body = merged[1]
            .content
            .as_ref()
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert!(
            body.contains("'Edit'"),
            "payload must name the tool: {body}"
        );
        assert!(
            body.contains("Retry"),
            "payload must tell the model to retry: {body}"
        );
        assert!(
            body.contains("old_stri"),
            "payload must include the raw fragment: {body}"
        );
        assert_eq!(merged[2].tool_use_id, "id_good_2");
        assert_eq!(merged[2].is_error, Some(false));
    }

    /// Integration-level proof that the whole §2 path produces the
    /// on-wire shape Anthropic expects: after a turn whose only
    /// tool_use had bad JSON, the next user message that `run_turn`
    /// would send contains a single `ContentBlock::ToolResult` with
    /// `is_error:true` and `tool_use_id` matching the placeholder. We
    /// assemble the user-message building blocks the same way
    /// `run_turn` does — but without needing a live ApiClient or
    /// network — by driving `drain_stream` and then `merge_tool_results`
    /// directly.
    #[tokio::test]
    async fn full_h1_path_builds_is_error_user_message_with_matching_id() {
        let (in_tx, in_rx) = mpsc::channel::<CcResult<StreamEvent>>(8);
        let cancel = CancellationToken::new();

        in_tx.send(Ok(message_start_event("msg_h1"))).await.unwrap();
        in_tx
            .send(Ok(content_block_start_tool_use(0, "tu_broken", "Bash")))
            .await
            .unwrap();
        in_tx
            .send(Ok(input_json_delta_event(0, r#"{"command":"ec"#)))
            .await
            .unwrap();
        drop(in_tx);

        let mut on_text = |_: &str| {};
        let mut text_buf = String::new();
        let (message, _usage, bad) =
            drain_stream(in_rx, &mut on_text, &mut text_buf, None, &cancel)
                .await
                .expect("drain ok");

        // Collect the tool_use blocks exactly like run_turn does.
        let tool_use_blocks: Vec<ToolUseBlock> = message
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolUse(tu) => Some(tu.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(tool_use_blocks.len(), 1);
        assert_eq!(tool_use_blocks[0].id, "tu_broken");

        // No valid_results — the only tool_use was bad.
        let tool_results = merge_tool_results(&tool_use_blocks, Vec::new(), &bad);
        assert_eq!(tool_results.len(), 1);
        let r = &tool_results[0];
        assert_eq!(r.tool_use_id, "tu_broken");
        assert_eq!(r.is_error, Some(true));

        // Shape the user-message the same way run_turn would, then
        // serialize it and verify the wire payload has the pair Claude
        // needs: `role:user` + `type:tool_result` + `is_error:true` +
        // matching `tool_use_id`.
        let user_msg = MessageParam {
            role: Role::User,
            content: MessageContent::Blocks(
                tool_results
                    .into_iter()
                    .map(ContentBlock::ToolResult)
                    .collect(),
            ),
        };
        let json = serde_json::to_value(&user_msg).unwrap();
        assert_eq!(json["role"], "user");
        let blocks = json["content"].as_array().expect("blocks array");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "tool_result");
        assert_eq!(blocks[0]["tool_use_id"], "tu_broken");
        assert_eq!(blocks[0]["is_error"], true);
    }
}
