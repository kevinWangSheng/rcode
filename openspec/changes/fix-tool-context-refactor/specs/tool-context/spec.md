# Spec — ToolContext and Tool::execute migration

## ADDED Requirements

### Requirement: Tool::execute takes a ToolContext reference

The `cc_core::Tool::execute` method MUST accept `ctx: &ToolContext`
in place of `cancel: &CancellationToken`. Every implementation of
`Tool` in the workspace — built-in tools under `cc-tools`, MCP
adapter tools under `cc-mcp::adapter`, and any driver-local tools
in `cc-agents` — SHALL migrate to the new signature in a single
commit. The bare-cancel signature SHALL NOT be preserved as a
compatibility shim.

`ToolContext` SHALL expose at minimum:

- `session: Arc<dyn SessionSink>` — session-side append hooks,
  reachable from inside the tool.
- `cancel: CancellationToken` — cooperative cancellation; tools
  SHOULD poll `ctx.cancel.is_cancelled()` in long-running loops
  identically to how they previously polled the bare token.
- `message_id: Option<String>` — assistant-turn message id that
  produced this tool_use. Present in cc-query dispatch; `None` in
  isolated tool tests.

Additional fields MAY be added additively in follow-up changes.
Existing fields SHALL NOT be removed or renamed without a spec
update.

#### Scenario: A tool observes its ctx

- **Given** a stub `Tool` implementation whose `execute` records
  the received `ctx` fields into a shared cell
- **When** cc-query dispatches the tool during a turn
- **Then** the recorded ctx has `cancel.is_cancelled() == false`
- **And** `session` is a sink pointing at the engine's current
  session
- **And** `message_id == Some(<assistant_turn_id>)`

#### Scenario: Cancel propagates identically

- **Given** a long-running tool that loops polling
  `ctx.cancel.is_cancelled()`
- **When** the caller cancels via `token.cancel()` on the token
  cloned into the ctx
- **Then** the next poll returns `true`
- **And** the tool exits its loop, matching pre-migration cancel
  semantics byte-for-byte

### Requirement: SessionSink exposes append methods without a dep cycle

The `cc_core::tool::SessionSink` trait SHALL define narrow
`append_interrupt_marker(&self, for_tool_use: bool) ->
CcResult<()>` and `append_file_history_snapshot(&self, snapshot:
&FileHistorySnapshot, is_update: bool) -> CcResult<()>` methods.
`cc_session::Session` SHALL `impl SessionSink` by delegating to its
existing inherent methods. `FileHistorySnapshot` and
`FileHistoryBackup` SHALL live in `cc-core` (relocated from
`cc-session`) so the trait is fully defined without cc-session in
scope.

`cc-core` SHALL NOT take a direct dependency on `cc-session`. The
sink trait is the only bridge.

#### Scenario: cc-core compiles without cc-session

- **Given** `Cargo.toml` dependency graph
- **When** `cargo check -p cc-core` runs
- **Then** cc-session is not in the transitive dependency set

#### Scenario: Session impl SessionSink round-trips a marker

- **Given** a Session in a temp dir
- **When** a stub tool holds `ctx.session.clone()` and calls
  `session.append_interrupt_marker(false)` from inside its
  `execute`
- **Then** the session's JSONL file contains a canonical interrupt
  marker entry identical to what
  `Session::append_interrupt_marker(false)` would have produced
  directly

### Requirement: QueryEngine holds Session behind Arc

`cc_query::engine::QueryEngine` SHALL store its session as
`Arc<Session>`. `QueryEngineConfig` SHALL accept `Arc<Session>`
from the caller. Callers (the `claude` binary, agent drivers,
tests) SHALL wrap their owned `Session` in `Arc::new(...)` at the
configuration site.

`Session` append methods take `&self`, so tool calls holding an
`Arc<Session>` clone can append without exclusive access.

#### Scenario: Two clones append concurrently

- **Given** a `QueryEngine` running a tool call whose `ctx.session`
  is a clone of the engine's session
- **When** both the engine's cancel path and the tool's snapshot
  emission call `session.append*` around the same time
- **Then** both appends succeed and the JSONL file contains both
  entries in some order
- **And** neither caller panics on lock contention

### Requirement: Every existing test passes unmodified

This change MUST be behaviour-neutral: every pre-change test SHALL continue to pass post-change with the same pass count per crate. Existing test counts across cc-core, cc-hooks, cc-query, cc-tools, cc-mcp, cc-agents, and the claude-cli binary SHALL remain unchanged. The only test-site edits allowed are mechanical: replacing bare `CancellationToken` parameters with `ToolContext::for_test_bare(token)` construction.

#### Scenario: Workspace test run reports identical counts

- **Given** a pre-change baseline of `cargo test --workspace`
  output with per-crate test counts
- **When** the post-change `cargo test --workspace` runs
- **Then** each crate reports the same number of passed tests as
  the baseline
- **And** no tests are ignored or deleted to force a match
