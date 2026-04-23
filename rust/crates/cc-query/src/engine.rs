use cc_api::{
    ApiClient, ContentBlockDelta, ContentBlockStartData, CreateMessageRequest, StreamAccumulator,
    StreamEvent, UsageTracker,
};
use cc_core::{
    AppEvent, CcError, CcResult, ContentBlock, Message, MessageContent, MessageParam,
    PermissionBehavior, PermissionPrompter, PromptDecision, Role, StopReason, SystemBlock,
    ToolContext, ToolResultBlock, ToolUseBlock, Usage,
};
use cc_hooks::{HookInput, HookRunner};
use cc_permissions::PermissionEngine;
use cc_session::{Session, INTERRUPT_MESSAGE, INTERRUPT_MESSAGE_FOR_TOOL_USE};
use cc_tools::ToolResult;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::cache_breakpoint::tag_last_block_for_caching;
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
    /// Extended-thinking setting forwarded to every `CreateMessageRequest`
    /// as the `thinking` field. `None` means "omit the field entirely"
    /// (server default; matches the pre-wiring behaviour byte-for-byte).
    pub thinking: Option<cc_core::ThinkingConfig>,
}

impl Default for QueryOptions {
    fn default() -> Self {
        QueryOptions {
            model: cc_core::models::DEFAULT.to_string(),
            max_tokens: 8192,
            non_interactive: false,
            bypass_permissions: false,
            thinking: None,
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
///
/// ## Message-level cache-breakpoint contract
///
/// `run_turn` tags the **trailing block of the trailing message** of every
/// outgoing `CreateMessageRequest` with `CacheControl::ephemeral_unscoped()`
/// via [`crate::cache_breakpoint::tag_last_block_for_caching`]. Callers
/// MUST NOT pre-tag message blocks themselves — double-tagging is benign
/// but misleading, and the engine's tag is always the authoritative cut.
/// TS parity: `services/api/claude.ts::addCacheBreakpoints`.
pub struct QueryEngine {
    api: ApiClient,
    tools: Arc<ToolRegistry>,
    permissions: PermissionEngine,
    hooks: Arc<HookRunner>,
    session: Arc<Session>,
    system_blocks: Vec<SystemBlock>,
    options: QueryOptions,
    prompter: Arc<dyn PermissionPrompter>,
    usage: UsageTracker,
    events_tx: Option<mpsc::Sender<AppEvent>>,
    /// Set to `true` when auto-compact fires inside the most recent `run_turn`.
    compacted_last_turn: bool,
    /// Contexts collected from PreToolUse hooks that must flow into the next
    /// API call as synthetic `MessageParam::user` entries. Drained at the top
    /// of each `run_turn` loop iteration so the strings are part of the
    /// `messages.clone()` snapshot the API request is built from.
    /// Mirrors TS `hooks.ts:2783-2788`.
    pending_additional_contexts: Vec<String>,

    /// Test-only scripted stream producer. When `Some(_)`, `run_turn` uses
    /// the closure to obtain the per-iteration `StreamEvent` receiver
    /// instead of calling `self.api.stream_message(..)`. Gated under
    /// `#[cfg(test)]` so release builds are byte-identical to pre-change.
    ///
    /// See `test_support::scripted_stream` for the common helper and
    /// `test_support::QueryEngine::with_stream_override` for the setter.
    #[cfg(test)]
    stream_override: Option<test_support::StreamOverrideFn>,
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
    /// Session wrapped in an `Arc` so `ToolContext::session` clones can
    /// drive the `SessionSink` trait from inside tool dispatches
    /// concurrently with the engine's own append calls.
    pub session: Arc<Session>,
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
            pending_additional_contexts: Vec::new(),
            #[cfg(test)]
            stream_override: None,
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

    /// Test-only accessor for the buffered PreToolUse contexts. Production
    /// code reads/writes via the `run_turn` drain; tests use this to assert
    /// the wiring without spinning up a full API mock.
    #[cfg(test)]
    pub(crate) fn pending_additional_contexts(&self) -> &[String] {
        &self.pending_additional_contexts
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
                self.pending_additional_contexts.clear();
                return Err(CcError::Other(format!("exceeded max turns ({MAX_TURNS})")));
            }

            debug!("query turn {turns}, messages={}", messages.len());

            // Drain any additional_context strings collected by PreToolUse
            // hooks in earlier iterations and inject them as user messages
            // BEFORE building the request so they're part of `messages.clone()`.
            // TS parity: `hooks.ts:2783-2788`.
            for ctx in self.pending_additional_contexts.drain(..) {
                let ctx_msg = MessageParam::user(ctx);
                self.session.append(&ctx_msg)?;
                messages.push(ctx_msg);
            }

            // Turn-level prompt-cache breakpoint: tag the trailing block of
            // the trailing message with `ephemeral` so everything up to
            // (but not including) the cut participates in cache reads on
            // the next turn. TS parity: `services/api/claude.ts::
            // addCacheBreakpoints`. Idempotent across loop iterations
            // (earlier tags live on those older messages; only the newest
            // message picks up a fresh tag every turn).
            tag_last_block_for_caching(messages);

            // 3. Build API request
            let mut req = CreateMessageRequest::new(&self.options.model, messages.clone())
                .with_max_tokens(self.options.max_tokens);

            if !self.system_blocks.is_empty() {
                req = req.with_system(self.system_blocks.clone());
            }
            if !tool_defs.is_empty() {
                req = req.with_tools(tool_defs.clone());
            }
            if let Some(cfg) = &self.options.thinking {
                req = req.with_thinking(cfg.clone());
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

            // Acquire the per-turn `StreamEvent` receiver.
            //
            // In test builds, if a scripted stream override is installed,
            // use it instead of the live API so run_turn-level regressions
            // can drive the full turn loop without a network call. The
            // `#[cfg]` gates ensure release builds compile to the same code
            // as before this change — no field, no branch, no cost.
            // See `test_support::scripted_stream` and
            // `test_support::QueryEngine::with_stream_override`.
            #[cfg(test)]
            let rx = if let Some(f) = &self.stream_override {
                f(&req, cancel)
            } else {
                self.api.stream_message(req, cancel).await?
            };
            #[cfg(not(test))]
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
            //
            // The marker wording comes from `cc_session::INTERRUPT_MESSAGE`, which
            // mirrors TS `utils/messages.ts::INTERRUPT_MESSAGE` byte-for-byte so
            // the resumed transcript parses identically under either runtime.
            if cancel.is_cancelled() && !text_buf.is_empty() {
                let mut interrupted_content = message.content.clone();
                interrupted_content.push(ContentBlock::text(format!("\n{INTERRUPT_MESSAGE}")));

                // The Anthropic API requires every `tool_use` block to be
                // followed in the next user message by a matching
                // `tool_result` block — otherwise the next request 400s
                // with `tool_use ids were found without tool_result blocks`.
                // When the user cancels mid-stream the model may have
                // already emitted one or more `tool_use` headers without
                // the engine getting a chance to execute them, so we
                // synthesize stub `tool_result` blocks that mark each
                // interrupted call as `is_error: true`. This keeps the
                // saved transcript replayable without losing the abort
                // signal: the next turn's request body is well-formed,
                // and the assistant sees that those tool calls were
                // cancelled by the user.
                let interrupted_tool_results: Vec<ContentBlock> = interrupted_content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::ToolUse(tu) => {
                            Some(ContentBlock::ToolResult(ToolResultBlock {
                                tool_use_id: tu.id.clone(),
                                content: Some(serde_json::Value::String(
                                    INTERRUPT_MESSAGE_FOR_TOOL_USE.to_string(),
                                )),
                                is_error: Some(true),
                                cache_control: None,
                            }))
                        }
                        _ => None,
                    })
                    .collect();

                let partial_msg = MessageParam {
                    role: Role::Assistant,
                    content: MessageContent::Blocks(interrupted_content),
                };
                self.session.append(&partial_msg)?;
                messages.push(partial_msg);

                let had_tool_use = !interrupted_tool_results.is_empty();
                if had_tool_use {
                    let result_msg = MessageParam {
                        role: Role::User,
                        content: MessageContent::Blocks(interrupted_tool_results),
                    };
                    self.session.append(&result_msg)?;
                    messages.push(result_msg);
                }

                // Canonical resume-detection entry: a standalone user message
                // carrying `INTERRUPT_MESSAGE` (or the tool-use variant). TS
                // resume logic keys off this entry to decide whether to prompt
                // for continuation. Write it AFTER the content blocks above so
                // a concurrent reader can't see a marker without the content
                // it refers to. Parity: TS `utils/messages.ts:` interrupt path.
                self.session.append_interrupt_marker(had_tool_use)?;

                self.pending_additional_contexts.clear();
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
                        self.execute_tools(&valid_blocks, &message.id, cancel)
                            .await?
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
    ///
    /// `assistant_msg_id` is the id of the assistant turn that emitted
    /// these `tool_use` blocks; it's threaded into every per-tool
    /// `ToolContext` as `message_id`. Follow-ups that need per-edit
    /// file-history snapshots read it back off the ctx.
    async fn execute_tools(
        &mut self,
        tool_use_blocks: &[ToolUseBlock],
        assistant_msg_id: &str,
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
                let session = Arc::clone(&self.session);
                let msg_id = assistant_msg_id.to_string();
                let futures: Vec<_> = authorized
                    .into_iter()
                    .map(|(tu, tool)| {
                        let cancel = cancel.child_token();
                        let events_tx = events_tx.clone();
                        let hooks = Arc::clone(&hooks);
                        let session_id = session_id.clone();
                        let session = Arc::clone(&session);
                        let msg_id = msg_id.clone();
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

                            let ctx = ToolContext {
                                session: session as Arc<dyn cc_core::SessionSink>,
                                cancel: cancel.clone(),
                                message_id: Some(msg_id),
                            };
                            let result: ToolResult = match tool
                                .execute(tu.input.clone(), &ctx)
                                .await
                            {
                                Ok(r) => r,
                                Err(e) => ToolResult::error(format!("Tool execution error: {e}")),
                            };

                            let tool_result_block = ToolResultBlock {
                                tool_use_id: tu.id.clone(),
                                content: Some(Value::String(result.content.clone())),
                                is_error: if result.is_error { Some(true) } else { None },
                                cache_control: None,
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
            results.push(self.execute_one_tool(tu, assistant_msg_id, cancel).await);
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

        // Carry any additional_context strings into the next API call. Done
        // BEFORE the block / async-rewake checks so a halting hook that also
        // emitted context doesn't lose the information — the next user-driven
        // turn (after the tool_result error) will still receive it. TS parity:
        // `hooks.ts:2783-2788`.
        self.pending_additional_contexts
            .extend(hook_result.additional_contexts.iter().cloned());

        // AsyncRewake takes precedence over the plain Block path so it can be
        // routed differently once the task-notification queue lands. For now
        // it surfaces as a tool_result error with a distinguishing prefix.
        // TS parity: `hooks.ts:1843-1875`.
        if let Some(rewake_msg) = hook_result.async_rewake.clone() {
            // TODO(batch-G task-notification-queue): replace this early
            // return with a queued re-entry once the infra lands.
            return Err(tool_result_error(
                &tu.id,
                format!("Hook requested async rewake: {rewake_msg}"),
            ));
        }

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
        assistant_msg_id: &str,
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
        let ctx = ToolContext {
            session: Arc::clone(&self.session) as Arc<dyn cc_core::SessionSink>,
            cancel: child_cancel.clone(),
            message_id: Some(assistant_msg_id.to_string()),
        };
        let result: ToolResult = match tool.execute(tu.input.clone(), &ctx).await {
            Ok(r) => r,
            Err(e) => ToolResult::error(format!("Tool execution error: {e}")),
        };

        let tool_result_block = ToolResultBlock {
            tool_use_id: tu.id.clone(),
            content: Some(Value::String(result.content.clone())),
            is_error: if result.is_error { Some(true) } else { None },
            cache_control: None,
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
        cache_control: None,
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
                    cache_control: None,
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
        cache_control: None,
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
                            cache_control: tr.cache_control.clone(),
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

/// Test-only scaffolding for driving `QueryEngine::run_turn` end-to-end
/// without a live API. See `openspec/changes/fix-engine-stream-mock-harness`
/// for the capability contract.
///
/// The module is `#[cfg(test)]`-gated so release builds contain neither
/// the `stream_override` field nor the branch that reads it. Tests inside
/// `cc-query` import `super::test_support::*` to get `scripted_stream`
/// plus the `with_stream_override` builder. Nothing here is meant to be
/// reachable from other crates — if cross-crate test reuse becomes a
/// need, promote to a `test-support` cargo feature.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// Closure type stored on `QueryEngine::stream_override`. Produces a
    /// fresh `mpsc::Receiver<CcResult<StreamEvent>>` on every invocation;
    /// each receiver stands in for what `ApiClient::stream_message` would
    /// have returned for that turn.
    pub(crate) type StreamOverrideFn = std::sync::Arc<
        dyn Fn(&CreateMessageRequest, &CancellationToken) -> mpsc::Receiver<CcResult<StreamEvent>>
            + Send
            + Sync,
    >;

    impl QueryEngine {
        /// Install a scripted stream producer. The closure is invoked
        /// once per `run_turn` iteration in place of
        /// `ApiClient::stream_message(..)`. Only available in test builds.
        pub(crate) fn with_stream_override<F>(mut self, f: F) -> Self
        where
            F: Fn(
                    &CreateMessageRequest,
                    &CancellationToken,
                ) -> mpsc::Receiver<CcResult<StreamEvent>>
                + Send
                + Sync
                + 'static,
        {
            self.stream_override = Some(std::sync::Arc::new(f));
            self
        }
    }

    /// Ship a canned `Vec` of events as a closure compatible with
    /// `with_stream_override`. Each invocation produces a fresh
    /// pre-loaded receiver — safe to call multiple times across turns.
    ///
    /// Error fidelity: `CcError` is not `Clone` (it wraps non-Clone
    /// `std::io::Error` and `serde_json::Error`), so scripted
    /// `Err(..)` entries are degraded to
    /// `Err(CcError::Other("scripted-stream error"))`. Tests that need
    /// a specific error variant SHOULD use `with_stream_override`
    /// directly with a stateful closure that constructs the error
    /// per-call.
    ///
    /// Events are pre-pushed into the channel synchronously before the
    /// receiver is returned (the channel capacity matches `events.len()`
    /// so the sends never block), which sidesteps the scheduler race
    /// that a spawn-and-send variant would have on a pre-cancelled
    /// token.
    pub(crate) fn scripted_stream(
        events: Vec<CcResult<StreamEvent>>,
    ) -> impl Fn(&CreateMessageRequest, &CancellationToken) -> mpsc::Receiver<CcResult<StreamEvent>>
           + Send
           + Sync
           + 'static {
        // Rebuild the script once into a form that can be cloned into
        // each receiver without losing ordering.
        let script: std::sync::Arc<Vec<CcResult<StreamEvent>>> = std::sync::Arc::new(
            events
                .into_iter()
                .map(|ev| match ev {
                    Ok(e) => Ok(e),
                    Err(_) => Err(CcError::Other("scripted-stream error".into())),
                })
                .collect(),
        );
        move |_req, _cancel| {
            let capacity = script.len().max(1);
            let (tx, rx) = mpsc::channel(capacity);
            for ev in script.iter() {
                // Re-wrap each event so every receiver gets its own
                // `CcResult<StreamEvent>` (StreamEvent is Clone; errors
                // were already degraded above).
                let cloned: CcResult<StreamEvent> = match ev {
                    Ok(e) => Ok(e.clone()),
                    Err(_) => Err(CcError::Other("scripted-stream error".into())),
                };
                // Channel is sized to fit the whole script, so
                // `try_send` cannot hit a Full error. A Closed error
                // is impossible too since we just created the rx.
                let _ = tx.try_send(cloned);
            }
            // Dropping `tx` at end of scope closes the channel so the
            // consumer sees `None` after the script is drained.
            rx
        }
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
            cache_control: None,
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
            cache_control: None,
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
            cache_control: None,
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
            cache_control: None,
        };
        let result = ToolResultBlock {
            tool_use_id: "tu_1".into(),
            content: Some(serde_json::Value::String("ok".into())),
            is_error: None,
            cache_control: None,
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
            cache_control: None,
        };
        let result = ToolResultBlock {
            tool_use_id: "tu_1".into(),
            content: Some(serde_json::Value::String("boom".into())),
            is_error: Some(true),
            cache_control: None,
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
            cache_control: None,
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
                cache_control: None,
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
            cache_control: None,
        };
        let tu_bad = ToolUseBlock {
            id: "id_bad".into(),
            name: "Edit".into(),
            input: serde_json::Value::Object(Default::default()),
            cache_control: None,
        };
        let tu_good_2 = ToolUseBlock {
            id: "id_good_2".into(),
            name: "Read".into(),
            input: serde_json::json!({"file_path":"/x"}),
            cache_control: None,
        };
        let original = vec![tu_good_1.clone(), tu_bad.clone(), tu_good_2.clone()];

        let valid_results = vec![
            ToolResultBlock {
                tool_use_id: "id_good_2".into(),
                content: Some(serde_json::Value::String("read ok".into())),
                is_error: Some(false),
                cache_control: None,
            },
            ToolResultBlock {
                tool_use_id: "id_good_1".into(),
                content: Some(serde_json::Value::String("hi".into())),
                is_error: Some(false),
                cache_control: None,
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

    // ---- PreToolUse consumer wiring tests (fix-hook-correctness-wiring) ----
    //
    // These exercise `check_tool_permissions` directly so we can assert the
    // additional_contexts / async_rewake plumbing without booting a mock API.
    // The ApiClient inside the engine is never actually called.

    use crate::prompter::StdinPrompter;
    use cc_api::AuthCredential;
    use cc_core::hook::HooksSettings;

    fn build_test_engine(hooks: HookRunner) -> QueryEngine {
        let api = ApiClient::new(
            reqwest::Client::new(),
            AuthCredential::ApiKey("sk-test".into()),
        );
        QueryEngine::new(QueryEngineConfig {
            api,
            tools: Arc::new(ToolRegistry::new()),
            permissions: PermissionEngine::default(),
            hooks: Arc::new(hooks),
            session: Arc::new(Session::new().expect("session")),
            system_blocks: Vec::new(),
            options: QueryOptions {
                bypass_permissions: true,
                ..QueryOptions::default()
            },
            prompter: Arc::new(StdinPrompter::new(true)),
        })
    }

    fn bash_tool_use() -> ToolUseBlock {
        ToolUseBlock {
            id: "tu_consumer".into(),
            name: "Bash".into(),
            input: serde_json::json!({"command": "echo hi"}),
            cache_control: None,
        }
    }

    #[tokio::test]
    async fn pretooluse_additional_contexts_buffered_for_next_turn() {
        // PreToolUse hook returns a structured response with
        // `additional_context`. Even though the hook does not block, the
        // engine must capture the context into `pending_additional_contexts`
        // so the next `run_turn` iteration injects it as a user message.
        let settings: HooksSettings = serde_json::from_str(
            r#"{
            "PreToolUse": [{"hooks": [{"type": "command", "command": "printf '{\"hook_specific_output\":{\"additional_context\":\"context-from-pre-tool-use\"}}'", "unsafe_shell": true}]}]
        }"#,
        )
        .unwrap();
        let hooks = HookRunner::new(&settings, reqwest::Client::new());
        let mut engine = build_test_engine(hooks);
        let cancel = CancellationToken::new();
        let tu = bash_tool_use();
        // The non-blocking hook lets check_tool_permissions return Ok; we
        // care about the side-effect on the buffer, not the tool that came
        // back. Tool resolution may still fail (Bash tool isn't registered),
        // so accept either branch and assert only on the buffer state.
        let _ = engine.check_tool_permissions(&tu, &cancel).await;
        assert_eq!(
            engine.pending_additional_contexts(),
            &["context-from-pre-tool-use".to_string()]
        );
    }

    #[tokio::test]
    async fn async_rewake_surfaces_as_distinct_tool_result_error() {
        // exit 2 + async_rewake: true should produce a tool_result error
        // whose content is prefixed with "Hook requested async rewake:" so
        // it's distinguishable from a plain Block (which uses "Blocked by
        // hook:"). The captured stdout becomes the rewake message.
        let settings: HooksSettings = serde_json::from_str(
            r#"{
            "PreToolUse": [{"hooks": [{"type": "command", "command": "printf 'wake-up-marker' && exit 2", "unsafe_shell": true, "async_rewake": true}]}]
        }"#,
        )
        .unwrap();
        let hooks = HookRunner::new(&settings, reqwest::Client::new());
        let mut engine = build_test_engine(hooks);
        let cancel = CancellationToken::new();
        let tu = bash_tool_use();
        let err = match engine.check_tool_permissions(&tu, &cancel).await {
            Ok(_) => panic!("async_rewake should short-circuit with an error block"),
            Err(e) => e,
        };
        assert_eq!(err.tool_use_id, "tu_consumer");
        assert_eq!(err.is_error, Some(true));
        let body = err
            .content
            .as_ref()
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert!(
            body.starts_with("Hook requested async rewake:"),
            "expected rewake prefix, got {body:?}"
        );
        assert!(
            body.contains("wake-up-marker"),
            "rewake message must include captured stdout, got {body:?}"
        );
    }

    #[test]
    fn thinking_omitted_when_options_unset() {
        // Default QueryOptions has `thinking = None`. The builder is only
        // called when the option is `Some(..)`, so the wire body has no
        // `thinking` key — matching today's behaviour byte-for-byte.
        let options = QueryOptions::default();
        let req = CreateMessageRequest::new(&options.model, Vec::<MessageParam>::new())
            .with_max_tokens(options.max_tokens);
        let req = if let Some(cfg) = &options.thinking {
            req.with_thinking(cfg.clone())
        } else {
            req
        };
        let json = serde_json::to_value(&req).unwrap();
        assert!(json.get("thinking").is_none());
    }

    #[test]
    fn thinking_enabled_budget_flows_to_request() {
        let options = QueryOptions {
            thinking: Some(cc_core::ThinkingConfig::Enabled {
                budget_tokens: 2048,
            }),
            ..QueryOptions::default()
        };
        let mut req = CreateMessageRequest::new(&options.model, Vec::<MessageParam>::new())
            .with_max_tokens(options.max_tokens);
        if let Some(cfg) = &options.thinking {
            req = req.with_thinking(cfg.clone());
        }
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(
            json["thinking"],
            serde_json::json!({"type": "enabled", "budget_tokens": 2048})
        );
    }

    #[test]
    fn thinking_adaptive_flows_to_request() {
        let options = QueryOptions {
            thinking: Some(cc_core::ThinkingConfig::Adaptive),
            ..QueryOptions::default()
        };
        let mut req = CreateMessageRequest::new(&options.model, Vec::<MessageParam>::new())
            .with_max_tokens(options.max_tokens);
        if let Some(cfg) = &options.thinking {
            req = req.with_thinking(cfg.clone());
        }
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["thinking"], serde_json::json!({"type": "adaptive"}));
    }

    #[test]
    fn request_body_carries_ephemeral_on_last_block() {
        // Integration test for the cache-breakpoint wiring: drive the same
        // `tag_last_block_for_caching(messages)` + `CreateMessageRequest::
        // new(..., messages.clone())` pipeline that `run_turn` uses, then
        // serialise and assert the wire shape. This sidesteps the missing
        // mock-API harness while still exercising the serde boundary.
        use crate::cache_breakpoint::tag_last_block_for_caching;
        use cc_core::{ContentBlock, MessageContent, Role, ToolUseBlock};

        let mut messages = vec![
            MessageParam {
                role: Role::User,
                content: MessageContent::Blocks(vec![ContentBlock::text("earlier")]),
            },
            MessageParam {
                role: Role::Assistant,
                content: MessageContent::Blocks(vec![
                    ContentBlock::text("prose"),
                    ContentBlock::ToolUse(ToolUseBlock {
                        id: "tu_x".into(),
                        name: "Bash".into(),
                        input: serde_json::json!({"command":"echo ok"}),
                        cache_control: None,
                    }),
                ]),
            },
        ];

        tag_last_block_for_caching(&mut messages);
        let req =
            CreateMessageRequest::new("claude-opus-4-7", messages.clone()).with_max_tokens(1024);
        let json = serde_json::to_value(&req).expect("serialize");

        let msgs = json["messages"].as_array().expect("messages array");
        assert_eq!(msgs.len(), 2);

        // Earlier message's block must not carry cache_control.
        let earlier_content = msgs[0]["content"].as_array().expect("blocks");
        assert!(
            earlier_content[0].get("cache_control").is_none(),
            "earlier block should not be tagged, got {earlier_content:?}"
        );

        // Last message's blocks: leading text untouched, trailing tool_use
        // carries the breakpoint.
        let last_content = msgs[1]["content"].as_array().expect("blocks");
        assert_eq!(last_content.len(), 2);
        assert!(
            last_content[0].get("cache_control").is_none(),
            "leading block of last message should not be tagged"
        );
        assert_eq!(
            last_content[1]["cache_control"]["type"],
            serde_json::json!("ephemeral"),
            "trailing block must carry ephemeral breakpoint, got {last_content:?}"
        );
    }

    #[tokio::test]
    async fn block_hook_uses_blocked_prefix_not_rewake_prefix() {
        // Plain `exit 2` (without async_rewake) must keep the historical
        // "Blocked by hook:" wording — the contrast against the rewake test
        // is what locks the two paths in as distinguishable.
        let settings: HooksSettings = serde_json::from_str(
            r#"{
            "PreToolUse": [{"hooks": [{"type": "command", "command": "printf 'no go' && exit 2", "unsafe_shell": true}]}]
        }"#,
        )
        .unwrap();
        let hooks = HookRunner::new(&settings, reqwest::Client::new());
        let mut engine = build_test_engine(hooks);
        let cancel = CancellationToken::new();
        let tu = bash_tool_use();
        let err = match engine.check_tool_permissions(&tu, &cancel).await {
            Ok(_) => panic!("block hook should short-circuit"),
            Err(e) => e,
        };
        let body = err
            .content
            .as_ref()
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert!(
            body.starts_with("Blocked by hook:"),
            "expected block prefix, got {body:?}"
        );
        assert!(
            !body.contains("async rewake"),
            "block path must not mention async rewake, got {body:?}"
        );
    }

    // ---- Engine stream-mock harness (fix-engine-stream-mock-harness) ----
    //
    // These tests exercise the test-only `with_stream_override` +
    // `scripted_stream` plumbing that lets run_turn-level regressions
    // drive the full turn loop without a live API. The `#[cfg(test)]`
    // gates ensure release builds are unaffected.

    fn content_block_stop(index: u32) -> StreamEvent {
        StreamEvent::ContentBlockStop { index }
    }

    fn message_stop_event() -> StreamEvent {
        StreamEvent::MessageStop
    }

    fn message_delta_end_turn() -> StreamEvent {
        StreamEvent::MessageDelta {
            delta: cc_api::MessageDeltaData {
                stop_reason: Some(cc_core::StopReason::EndTurn),
                stop_sequence: None,
            },
            usage: cc_api::MessageDeltaUsage { output_tokens: 1 },
        }
    }

    #[tokio::test]
    async fn stream_override_drives_full_turn() {
        // §4.1 smoke test: build an engine with a scripted stream that
        // walks the full well-formed sequence (MessageStart +
        // ContentBlockStart/Delta/Stop + MessageDelta(end_turn) +
        // MessageStop) and confirm `run_turn` returns the delta text.
        //
        // This is the "does the override plumbing actually reach
        // drain_stream" proof — nothing more.
        use super::test_support::scripted_stream;
        let hooks = HookRunner::new(&HooksSettings::default(), reqwest::Client::new());
        let engine = build_test_engine(hooks).with_stream_override(scripted_stream(vec![
            Ok(message_start_event("msg_ok")),
            Ok(content_block_start_text(0)),
            Ok(text_delta_event("hi")),
            Ok(content_block_stop(0)),
            Ok(message_delta_end_turn()),
            Ok(message_stop_event()),
        ]));
        let mut engine = engine;
        let cancel = CancellationToken::new();
        let mut messages = Vec::new();
        let final_text = engine
            .run_turn("hello", |_| {}, &mut messages, &cancel)
            .await
            .expect("run_turn should succeed");
        assert_eq!(final_text, "hi");
        // No pending contexts should linger after a clean turn.
        assert!(engine.pending_additional_contexts().is_empty());
    }

    #[tokio::test]
    async fn stream_override_respects_cancel() {
        // §4.2 smoke test: cancel the token before `run_turn`, script a
        // MessageStart + one text delta. `drain_stream` itself doesn't
        // poll the cancel token inside its recv loop, so the cancel
        // branch in `run_turn` only trips when `text_buf` is non-empty.
        // We therefore script at least one delta so the partial-save
        // path fires and we get the expected `CcError::Cancelled`.
        //
        // (The stronger "cancel during drain_stream recv" contract is
        // already covered by drain_stream's own tests — this one is
        // about the harness not hanging.)
        use super::test_support::scripted_stream;
        let hooks = HookRunner::new(&HooksSettings::default(), reqwest::Client::new());
        let mut engine = build_test_engine(hooks).with_stream_override(scripted_stream(vec![
            Ok(message_start_event("msg_cancel")),
            Ok(content_block_start_text(0)),
            Ok(text_delta_event("partial")),
            Ok(content_block_stop(0)),
            Ok(message_stop_event()),
        ]));
        let cancel = CancellationToken::new();
        cancel.cancel();
        let mut messages = Vec::new();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            engine.run_turn("hello", |_| {}, &mut messages, &cancel),
        )
        .await
        .expect("run_turn must not hang under the harness");
        assert!(
            matches!(result, Err(CcError::Cancelled)),
            "expected Err(Cancelled), got {result:?}"
        );
    }

    #[tokio::test]
    async fn additional_contexts_are_cleared_on_cancel() {
        // §5.1 smoke / unblock of fix-hook-correctness-wiring §5.3.
        //
        // Prior to this harness the cancel branch in `run_turn` was
        // unreachable from tests (only the drain_stream path was
        // directly exercisable). Now we can populate
        // `pending_additional_contexts` manually, drive one cancelled
        // turn, and assert the buffer is drained by the cancel branch
        // at engine.rs:~338.
        use super::test_support::scripted_stream;
        let hooks = HookRunner::new(&HooksSettings::default(), reqwest::Client::new());
        let mut engine = build_test_engine(hooks).with_stream_override(scripted_stream(vec![
            Ok(message_start_event("msg_clear")),
            Ok(content_block_start_text(0)),
            Ok(text_delta_event("partial")),
            Ok(content_block_stop(0)),
            Ok(message_stop_event()),
        ]));
        // Seed the buffer with a context as if a PreToolUse hook had
        // produced one on a previous iteration.
        engine
            .pending_additional_contexts
            .push("stale-context".into());
        let cancel = CancellationToken::new();
        cancel.cancel();
        let mut messages = Vec::new();
        let result = engine
            .run_turn("hello", |_| {}, &mut messages, &cancel)
            .await;
        assert!(matches!(result, Err(CcError::Cancelled)));
        assert!(
            engine.pending_additional_contexts().is_empty(),
            "cancel branch must clear pending contexts, found {:?}",
            engine.pending_additional_contexts()
        );
    }

    #[tokio::test]
    async fn scripted_stream_produces_independent_receivers() {
        // Spec scenario: a `scripted_stream(vec![Ok(MessageStart)])`
        // closure SHALL yield a fresh, independent receiver on each
        // invocation. Guard against accidental shared-state
        // regressions (e.g. a single channel reused across calls).
        use super::test_support::scripted_stream;
        let producer = scripted_stream(vec![Ok(message_start_event("msg_multi"))]);
        let req = CreateMessageRequest::new("claude-test", Vec::<MessageParam>::new());
        let cancel = CancellationToken::new();
        let mut rx1 = producer(&req, &cancel);
        let mut rx2 = producer(&req, &cancel);
        let e1 = rx1
            .recv()
            .await
            .expect("rx1 yields its MessageStart")
            .expect("Ok event");
        let e2 = rx2
            .recv()
            .await
            .expect("rx2 yields its MessageStart")
            .expect("Ok event");
        assert!(matches!(e1, StreamEvent::MessageStart { .. }));
        assert!(matches!(e2, StreamEvent::MessageStart { .. }));
        // Both receivers should be drained after their single event.
        assert!(rx1.recv().await.is_none());
        assert!(rx2.recv().await.is_none());
    }
}
