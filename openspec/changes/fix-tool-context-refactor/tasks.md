## 1. cc-core — types and trait

- [ ] 1.1 Decide: `SessionSink` trait in `cc-core::tool` (break dep
      cycle) vs. `Arc<Session>` direct field (requires moving
      `ToolContext` to `cc-tools`). Proposal recommends sink trait;
      document the decision in the commit message.
- [ ] 1.2 If sink trait: define
      ```rust
      #[async_trait::async_trait]
      pub trait SessionSink: Send + Sync {
          fn append_interrupt_marker(&self, for_tool_use: bool)
              -> CcResult<()>;
          fn append_file_history_snapshot(
              &self,
              snapshot: &FileHistorySnapshot,
              is_update: bool,
          ) -> CcResult<()>;
      }
      ```
      Place in `rust/crates/cc-core/src/tool.rs`. Keep the trait
      minimal — extend only when a caller actually needs more.
- [ ] 1.3 Move `FileHistorySnapshot` / `FileHistoryBackup` (and any
      helper types) from `cc_session::lib` to `cc_core::message` or
      a new `cc_core::file_history` module. Re-export from
      `cc-session` at their old path so external callers don't
      break. Tests that locate the types via `cc_session::
      FileHistorySnapshot` should keep working.
- [ ] 1.4 Add `ToolContext` struct at
      `rust/crates/cc-core/src/tool.rs`:
      ```rust
      #[derive(Clone)]
      pub struct ToolContext {
          pub session: Arc<dyn SessionSink>,
          pub cancel: CancellationToken,
          pub message_id: Option<String>,
      }
      ```
- [ ] 1.5 Add `pub fn for_test_bare(cancel: CancellationToken) ->
      Self` in a `ToolContext::test_support` impl block, gated on
      `#[cfg(test)]`. The returned ctx holds a noop `SessionSink`
      implementation whose methods return `Ok(())` — fine for
      tests that don't care about session side effects.
- [ ] 1.6 Change the `Tool::execute` signature at `cc-core/src/
      tool.rs:75` from `execute(&self, input: Value, cancel:
      &CancellationToken)` to `execute(&self, input: Value, ctx:
      &ToolContext)`. Return type `CcResult<ToolResult>`
      unchanged. Do NOT deprecate the old signature — remove it
      cleanly so callers must migrate.

## 2. cc-session — implement SessionSink

- [ ] 2.1 Add `impl SessionSink for Session` in
      `rust/crates/cc-session/src/lib.rs`. The two methods delegate
      to the existing `append_interrupt_marker` /
      `append_file_history_snapshot` inherent methods.
- [ ] 2.2 Verify `Session` is `Send + Sync` (it already is — all
      fields are or wrap thread-safe primitives).

## 3. cc-tools — migrate every Tool::execute

Each file below gets the same mechanical edit: replace `cancel:
&CancellationToken` → `ctx: &ToolContext`; replace `cancel` usages
inside the body with `&ctx.cancel`; no other behaviour change.

- [ ] 3.1 `agent_tool.rs`
- [ ] 3.2 `ask_user_question.rs`
- [ ] 3.3 `bash.rs` — the module uses `cancel.is_cancelled()` in
      multiple spots; swap each to `ctx.cancel.is_cancelled()` or
      `&ctx.cancel` for pass-through.
- [ ] 3.4 `edit.rs`
- [ ] 3.5 `enter_plan_mode.rs`
- [ ] 3.6 `enter_worktree.rs`
- [ ] 3.7 `exit_plan_mode.rs`
- [ ] 3.8 `exit_worktree.rs`
- [ ] 3.9 `glob_tool.rs`
- [ ] 3.10 `grep.rs`
- [ ] 3.11 `read.rs`
- [ ] 3.12 `send_message.rs`
- [ ] 3.13 `sleep_tool.rs`
- [ ] 3.14 `task_create.rs`
- [ ] 3.15 `task_get.rs`
- [ ] 3.16 `task_list.rs`
- [ ] 3.17 `task_output.rs`
- [ ] 3.18 `task_stop.rs`
- [ ] 3.19 `task_update.rs`
- [ ] 3.20 `team_create.rs`
- [ ] 3.21 `team_delete.rs`
- [ ] 3.22 `todo_write.rs`
- [ ] 3.23 `tool_search.rs`
- [ ] 3.24 `web_fetch/` (top-level mod + any submodules)
- [ ] 3.25 `web_search.rs`
- [ ] 3.26 `write.rs`
- [ ] 3.27 In-file unit tests in each of the above: swap
      `tool.execute(input, &cancel)` → `tool.execute(input, &ctx)`
      where `let ctx = ToolContext::for_test_bare(cancel)`.

## 4. cc-mcp — migrate adapter

- [ ] 4.1 `rust/crates/cc-mcp/src/adapter.rs`: the `Tool` impl
      for MCP-exposed tools. Same mechanical signature change; no
      behaviour change. Forward `ctx.cancel` to the MCP call.

## 5. cc-query — session behind Arc + ctx construction

- [ ] 5.1 Change the struct field at `rust/crates/cc-query/src/
      engine.rs:61-74` from `session: Session` to `session:
      Arc<Session>`.
- [ ] 5.2 Update `QueryEngineConfig` at `engine.rs:81-...` to
      accept `Arc<Session>` instead of `Session`.
- [ ] 5.3 At every `self.session.append*` call site, confirm it
      still compiles (Arc derefs should make this transparent;
      `Session::append*` takes `&self`).
- [ ] 5.4 In the tool-dispatch site (find via `rg -n
      "tool.execute\(" crates/cc-query/src/engine.rs`), construct:
      ```rust
      let ctx = ToolContext {
          session: self.session.clone(),
          cancel: cancel.clone(),
          message_id: Some(message.id.clone()),
      };
      ```
      where `message.id` comes from the stream-accumulator-produced
      `Message` already in scope. If `message.id` is not yet
      accessible at that point, pull it from the match arm above
      (`engine.rs` around line 280 where `message.content` is
      iterated for tool_use blocks).
- [ ] 5.5 Pass `&ctx` to the `.execute(...)` call. Drop the bare
      `&cancel` argument.

## 6. cc — wrap Session in Arc at construction

- [ ] 6.1 In `rust/cc/src/main.rs` find the `Session::resume` /
      `Session::new` call and `Session::new_for_task` equivalents;
      wrap the owned `Session` in `Arc::new(...)` before passing to
      `QueryEngineConfig`.

## 7. cc-agents — migrate driver call sites

- [ ] 7.1 `rg -n "\.execute\(" crates/cc-agents/` — find every
      `Tool::execute` caller. Build `ToolContext` at each site with
      the driver's own session handle (leader session for
      `run_local_agent`; teammate session for
      `run_in_process_teammate`).
- [ ] 7.2 If a driver has no session handle, create one via the
      existing `Session::new_for_task` pattern or pass
      `Session::noop_for_driver()` (add one as a trivial helper if
      needed — document in commit message).

## 8. Integration tests

- [ ] 8.1 Global call-site scan: `rg -n "\.execute\(" rust/` —
      every hit must either (a) be a `Tool::execute` already
      migrated or (b) be unrelated (some other `execute` method).
      Zero `(input, &cancel)` 2-arg calls left.
- [ ] 8.2 Individual-tool integration tests (anything in
      `rust/crates/cc-tools/tests/`, if present): migrate identically.

## 9. Verification

- [ ] 9.1 `cargo fmt --all` clean.
- [ ] 9.2 `cargo clippy --workspace --all-targets -- -D warnings`
      clean.
- [ ] 9.3 `cargo test --workspace` — all green, same counts as
      before the change (no new or deleted tests). Flag any delta
      in the commit message.
- [ ] 9.4 No `unsafe` blocks added. No `Arc` / `Mutex` beyond the
      one `Arc<Session>` change.

## 10. Sign-off

- [ ] 10.1 Commit message references
      `fix-session-resume-wiring` original §2 as the origin, and
      marks this as the unblocker for
      `fix-file-history-snapshot-producers`.
- [ ] 10.2 Update memory note `project_phase3_progress.md` to flag
      that `Tool::execute` now takes `&ToolContext`, for future
      tool-authoring sessions.
- [ ] 10.3 Do NOT mark P0 #5 / #6 closed yet — those flip only
      after `fix-file-history-snapshot-producers` lands.
