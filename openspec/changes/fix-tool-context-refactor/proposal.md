# Proposal — Introduce cc_core::ToolContext and migrate Tool::execute

## Why

`fix-session-resume-integrity` shipped `Session::
append_file_history_snapshot` and
`Session::append_interrupt_marker` helpers that the QA pass on
2026-04-23 confirmed have zero production callers. Edit / Write
tools need session access to emit pre-mutation snapshots, but the
current `Tool` trait offers no way to reach it:

```rust
// rust/crates/cc-core/src/tool.rs:75
async fn execute(&self, input: Value, cancel: &CancellationToken)
    -> CcResult<ToolResult>;
```

The only argument tools receive besides `input` is a cancellation
token. Adding the session as a second bare argument would double the
migration cost on the next feature that needs context (P0 #13
`Read` device-blocklist, P1 #32 structured Edit/Write output, P2 #44
file-history + LSP diagnostic clearing, P2 #59 `apiKeyHelper`
sanitisation). The right move is one struct that can grow without
further trait-signature changes.

Splitting this refactor out of `fix-session-resume-wiring` is
deliberate: the signature change cascades across ~28 tool files and
every tool test site in the workspace. Landing it as a clean,
behaviour-neutral commit makes the diff reviewable and de-risks
`fix-file-history-snapshot-producers`, which otherwise would have to
combine "refactor 28 files" with "add snapshot emission semantics"
in a single commit.

## Goal

Swap `Tool::execute`'s `cancel: &CancellationToken` parameter for a
`ctx: &ToolContext` bundle that carries session, cancel token, and a
`message_id` hint. Update every tool and call site in lockstep. Do
not add new behaviour; existing tests must pass unmodified (aside
from call-site construction of `ToolContext`).

Not in scope:

- Edit / Write snapshot emission — follow-up
  `fix-file-history-snapshot-producers`.
- Any new `ctx` fields beyond the three listed below. The struct is
  intentionally narrow so the migration stays mechanical.
- Reading `ctx.message_id` from anywhere. This change threads a
  reasonable value in from the engine but no tool consumes it yet.
- MCP-adapter tools (`cc-mcp::adapter`): they implement `Tool` too
  and get the same signature migration, but no MCP-specific
  semantics change.

## What changes

### 1. `cc_core::tool::ToolContext`

```rust
// rust/crates/cc-core/src/tool.rs (new, near the existing Tool trait)
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Runtime dependencies a tool needs beyond its `input` JSON.
///
/// Passed by reference to every `Tool::execute` call. New fields
/// may be added additively; do not remove fields.
#[derive(Clone)]
pub struct ToolContext {
    /// Handle to the caller's session, so tools can append
    /// side-effect entries (file-history snapshots, interrupt
    /// markers, etc.).
    pub session: Arc<cc_session::Session>,

    /// Cooperative cancellation. Tools should poll
    /// `ctx.cancel.is_cancelled()` in long-running loops.
    pub cancel: CancellationToken,

    /// The assistant-turn message id that produced this tool_use.
    /// Currently populated from `cc-query` dispatch (best-effort);
    /// `None` in isolated tool tests.
    pub message_id: Option<String>,
}
```

Note the `cc_session::Session` reference in `cc_core`. `cc-core`
currently does not depend on `cc-session`. Two options:

- **Option A**: add `cc-session` as a dependency of `cc-core`.
  `cc-session` already depends on `cc-core` for `CcResult` /
  `Message` types, so this would create a cycle. Rejected.
- **Option B** (adopt): make `ToolContext::session` generic over a
  trait, or put `ToolContext` in a crate that sits between cc-core
  and cc-session. Simplest concrete path: move `ToolContext` into
  `cc-tools` (not `cc-core`), which already depends on cc-session,
  and re-export it from `cc-core::tool` via a type alias if callers
  want a core-shaped path.

  Alternatively: keep `ToolContext` in `cc-core` but have its
  `session` field hold `Arc<dyn SessionSink>` where `SessionSink` is
  a narrow trait with just `append_interrupt_marker` /
  `append_file_history_snapshot` methods implemented by
  `cc_session::Session`. This lets `cc-core` stay cc-session-
  ignorant.

  **Recommended**: the sink-trait path. Keep `ToolContext` in
  `cc-core` so the `Tool` trait can reference it without a dep
  inversion. Define:

  ```rust
  // rust/crates/cc-core/src/tool.rs
  #[async_trait::async_trait]
  pub trait SessionSink: Send + Sync {
      fn append_interrupt_marker(&self, for_tool_use: bool)
          -> CcResult<()>;
      fn append_file_history_snapshot(
          &self,
          snapshot: &FileHistorySnapshot,
          is_update: bool,
      ) -> CcResult<()>;
      // … extend as follow-ups need more methods
  }
  ```

  And move `FileHistorySnapshot` / `FileHistoryBackup` types to
  `cc-core::message` (they are plain serde structs; zero runtime
  behaviour). `cc_session::Session` then `impl SessionSink` by
  delegating to its existing methods.

  Pros: no dep cycle, tools can be unit-tested with a trivial mock
  sink, cc-core remains the contract hub.
  Cons: one extra layer of indirection; callers have to `.clone()`
  or `Arc<dyn SessionSink>` instead of `Arc<Session>` directly.

  Go with this. The indirection cost is negligible because the only
  two call sites that write through the sink (Edit + Write) do it
  once per invocation.

  If `SessionSink` turns out to be too much abstraction for just
  two methods, fall back to making `ToolContext` live in `cc-tools`
  and re-exporting `Tool` from there; cc-core gets the struct via
  a re-export. Document the chosen path in the commit message.

### 2. `Tool::execute` signature

```rust
// rust/crates/cc-core/src/tool.rs:75
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    // existing name(), description(), input_schema(), is_read_only()

    async fn execute(&self, input: Value, ctx: &ToolContext)
        -> CcResult<ToolResult>;

    // to_definition() unchanged
}
```

### 3. Migration across tools

Tools in `rust/crates/cc-tools/src/`:

```
agent_tool.rs         glob_tool.rs       task_output.rs
ask_user_question.rs  grep.rs            task_stop.rs
bash.rs               read.rs            task_update.rs
edit.rs               send_message.rs    team_create.rs
enter_plan_mode.rs    sleep_tool.rs      team_delete.rs
enter_worktree.rs     task_create.rs     todo_write.rs
exit_plan_mode.rs     task_get.rs        tool_search.rs
exit_worktree.rs      task_list.rs       web_fetch/
                                         web_search.rs
                                         write.rs
```

plus MCP adapter: `rust/crates/cc-mcp/src/adapter.rs`.

Mechanical edit per tool:

- Change `execute(&self, input: Value, cancel: &CancellationToken)`
  to `execute(&self, input: Value, ctx: &ToolContext)`.
- Any function-body reference to `cancel` becomes `&ctx.cancel`.
  Count: TaskStop / TaskOutput / Bash / Grep / Read / Glob /
  WebFetch / WebSearch all call `cancel.is_cancelled()` or pass
  `cancel` to a downstream helper. Replace each with `&ctx.cancel`.
- Unit tests inside each tool file pass a bare `CancellationToken`.
  Build a throwaway helper `ToolContext::for_test_bare(cancel)` in
  `cc-core::tool::test_support` that constructs a ctx with a
  noop-sink session stand-in, so test sites can migrate with a
  single-line change.

### 4. `QueryEngine` — session behind Arc

`engine.rs` today owns `session: Session`. Change to `session:
Arc<Session>` in the struct (`engine.rs:61-74`). `QueryEngineConfig`
accepts `Arc<Session>` from the caller (cc/main.rs wraps its owned
`Session` at construction time).

All existing `self.session.append(...)` call sites still work
because `Session::append*` takes `&self`. No deref gymnastics.

At the tool-dispatch site (`engine.rs` inside the tool-use match),
build a ctx per call:

```rust
let ctx = ToolContext {
    session: self.session.clone(),                 // Arc<Session>
    cancel: cancel.clone(),
    message_id: Some(assistant_msg_id.clone()),
};
tool.execute(tu.input.clone(), &ctx).await
```

`assistant_msg_id` is already available at that point (it's the id
attached to the message that emitted the `tool_use` block; cc-query
today reads it from the stream accumulator via
`message.id`).

### 5. cc-agents driver

`cc-agents` also invokes tools (via `Tool::execute`) when running
LocalShell / InProcess teammates. Update those call sites to build a
`ToolContext` from whatever session is available in that driver —
for teammate tasks the session is the teammate's child session, not
the leader's. Thread from `TaskRegistry` / `InProcessRunner` as
needed. If a driver has no session handle today, create a new
unnamed session via existing `Session::new_for_task` (or its
equivalent) so the ctx is always populated.

### 6. Integration tests

Search for every `.execute(` call site across the workspace:

```
rg -n "\.execute\(" rust/
```

Each call needs a `ToolContext`. The test helper
`ToolContext::for_test_bare(cancel)` keeps the migration to one line
per site. Do NOT add `#[allow(deprecated)]` shims or split the
signature change across two commits — the cargo build must stay
green at every commit boundary.

## Impact

- **Affected specs**: new `tool-context` capability (this change's
  spec). No existing spec modifications.
- **Affected crates**:
  - `cc-core` — new `ToolContext` struct + `SessionSink` trait +
    `FileHistorySnapshot` type relocation (if chosen).
  - `cc-session` — `impl SessionSink for Session`.
  - `cc-tools` — 28 tool migrations.
  - `cc-mcp` — adapter migration.
  - `cc-query` — struct field `session: Arc<Session>` + dispatch
    site builds ctx.
  - `cc-agents` — tool dispatch sites thread ctx.
  - `cc` (claude binary) — wraps owned `Session` in `Arc` before
    handing to `QueryEngine::new`.
- **Compatibility**: breaking change to the internal `Tool` trait.
  No stable public API consumers yet. All workspace callers
  migrate in the same commit.
- **Behaviour**: zero change. Every existing tool test passes
  unmodified (modulo call-site ctx construction).

## Open questions

1. `SessionSink` trait vs. `Arc<Session>` direct field: the
   proposal recommends the trait to break the cc-core ↔ cc-session
   dep cycle. If the trait's surface area feels wrong once the
   follow-ups land, revisit.
2. `message_id` source: cc-query reads the assistant message id
   from the stream accumulator. If that id is unstable between
   drain_stream variants, plumb it from `message.id` of the
   `Message` constructed at `engine.rs` around line 211-218
   instead.
3. `ToolContext::for_test_bare` lives in `cc-core::tool` as a
   `#[cfg(test)]` or `#[cfg(any(test, feature = "test-support"))]`
   helper. Pick whichever matches the existing crate feature style
   (check `Cargo.toml` for `test-support` or similar — if absent,
   gate on `#[cfg(test)]` only and reach via
   `cc_core::tool::test_support`).
