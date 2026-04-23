# Proposal — Wire interrupt-marker and file-history-snapshot producers

## Why

`fix-session-resume-integrity` (commits `20c0c3f` + `b0208be`,
2026-04-23) added three pieces of machinery:

1. `INTERRUPT_MESSAGE` / `INTERRUPT_MESSAGE_FOR_TOOL_USE` pub consts
   matching TS literal wording.
2. `Session::append_interrupt_marker(for_tool_use: bool)` which
   writes the marker as a user message through the durable append
   path.
3. `FileHistorySnapshot` / `FileHistoryBackup` types +
   `Session::append_file_history_snapshot(...)` +
   `Session::file_history_snapshots()` reader.

The QA pass on 2026-04-23 searched the codebase for callers:

```
$ rg -n "append_interrupt_marker|append_file_history_snapshot" \
     rust/crates rust/cc | grep -v test
# — zero hits —
```

Meanwhile `rust/crates/cc-query/src/engine.rs:222-269` still writes
the partial-interrupt marker as a **hard-coded
`"\n[Interrupted by user]"`** literal baked into the assistant's
content block (line 224) + into synthetic `tool_result` error
strings (line 246). That wording differs from the TS canonical
`"[Request interrupted by user]"` / `"[Request interrupted by user
for tool use]"` and bypasses the new `Session` helper entirely.

Edit/Write tools at `rust/crates/cc-tools/src/{edit,write}.rs` never
snapshot anything — so resuming a session after a file edit loses
the pre-edit state that TS would have persisted via a
`file-history-snapshot` entry (TS `utils/fileHistory.ts:39-52`).

End result: parity gaps **P0 #5** (interrupt marker) and **P0 #6**
(file-history snapshot persistence) are **still open** end-to-end —
the spec lets the machinery be built, but no caller is using it.

## Goal

Wire the two producers so resumed sessions actually preserve:

- interrupt markers (both the plain and tool-use variants)
- file-history snapshots on Edit / Write success

while keeping the existing in-turn correctness fixes (the synthetic
`tool_result` stub for each dangling `tool_use` on cancel — see
`engine.rs:239-254`).

Not in scope:

- Migrating all other `[Interrupted …]` literal sites (the TUI's
  status-bar label at `cc-tui/src/render.rs:609` is user-facing text
  for an unrelated button and should stay distinct).
- Threading a richer tool context that would let non-file-editing
  tools also talk back to the session. Edit and Write are the only
  tools TS snapshots from; copy that scope exactly.
- Per-edit git-diff capture. File-history snapshots only carry
  pre-edit contents, not diffs.

## What changes

### 1. Interrupt marker — cc-query cancel path

**Current** (`engine.rs:222-270`):

```rust
if cancel.is_cancelled() && !text_buf.is_empty() {
    let mut interrupted_content = message.content.clone();
    interrupted_content.push(ContentBlock::text("\n[Interrupted by user]"));
    // …
    let interrupted_tool_results: Vec<ContentBlock> = interrupted_content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::ToolUse(tu) => Some(ContentBlock::ToolResult(ToolResultBlock {
                tool_use_id: tu.id.clone(),
                content: Some(Value::String("[Interrupted by user]".to_string())),
                is_error: Some(true),
                cache_control: None,
            })),
            _ => None,
        })
        .collect();
    // … self.session.append(&partial_msg); self.session.append(&result_msg);
    return Err(CcError::Cancelled);
}
```

**Target**:

1. Replace the literal `"\n[Interrupted by user]"` on line 224 with
   a prefix using the canonical const:
   ```rust
   use cc_session::INTERRUPT_MESSAGE;
   interrupted_content.push(ContentBlock::text(
       format!("\n{}", INTERRUPT_MESSAGE),
   ));
   ```
2. Replace the literal `"[Interrupted by user]"` inside the
   `tool_result` stub on line 246 with `INTERRUPT_MESSAGE_FOR_TOOL_USE`:
   ```rust
   use cc_session::INTERRUPT_MESSAGE_FOR_TOOL_USE;
   content: Some(Value::String(
       INTERRUPT_MESSAGE_FOR_TOOL_USE.to_string(),
   )),
   ```
3. After the existing `self.session.append(&partial_msg)?` and
   (when applicable) `self.session.append(&result_msg)?` calls
   (lines 260-269), add a call to the new helper that writes a
   canonical standalone marker entry for resume detection:
   ```rust
   // Canonical resume-detection entry (TS utils/messages.ts parity).
   self.session.append_interrupt_marker(!interrupted_tool_results.is_empty())?;
   ```
   Pass `true` when dangling tool_uses existed (tool-use variant of
   the marker); `false` otherwise.

   Rationale for writing both the assistant-content marker (step 1)
   AND the standalone marker: TS resume logic inspects the latest
   entries to decide whether to prompt for a resume-continuation
   message; the standalone entry is what that check keys on. The
   assistant-content marker is what the next-turn API request needs
   so the stream looks well-formed.

### 2. File-history snapshot — Edit and Write tools

The Tool trait today takes only `(input, cancel)`. Edit / Write need
a `Session` handle to call `append_file_history_snapshot`. Two
options:

**Option A (preferred): tool context struct.** Add a
`ToolContext` to `cc-core::tool` carrying whatever a tool might need
that's currently global:

```rust
pub struct ToolContext {
    pub session: Arc<Session>,
    pub cancel: CancellationToken,
    // (future: pub project_dir: Arc<PathBuf>, pub config: Arc<Settings>)
}

#[async_trait]
pub trait Tool: Send + Sync {
    // existing fn name(), description(), input_schema(), is_read_only()
    async fn execute(&self, input: Value, ctx: &ToolContext)
        -> CcResult<ToolResult>;
}
```

This is a breaking change to every tool in `cc-tools/src/*`. Count
is ~28 tools. Most just replace `cancel: &CancellationToken` →
`ctx: &ToolContext` and deref `&ctx.cancel` where they currently
use `cancel`. The cc-query dispatch site (`engine.rs` tool-call
match) builds the `ToolContext` once per call.

**Option B (narrow): session-aware tool helper.** Keep the `Tool`
trait as-is. Add a separate `SessionAwareTool` trait implemented
only by Edit and Write that takes `session: &Session` as an extra
parameter, and dispatch to it from cc-query when the tool impls it.
Less disruptive but sticks an awkward branch in the engine.

Recommend **Option A** because other future audits (P0 #13 Read
`pages` / device blocklist, P1 #32 structured Edit/Write output,
P2 #44 file-history + LSP diagnostic clearing + skill activation)
all want extra context that doesn't belong on the input JSON. Doing
A once is cheaper than threading one-off params through a dozen
tools.

Either way, the producers look like:

```rust
// Before mutating write in write.rs / edit.rs, snapshot if the file
// exists (no snapshot for net-new files — matches TS).
if path.exists() {
    let backup = FileHistoryBackup {
        backup_file_name: relative_path.to_string_lossy().into(),
        backup_time: SystemTime::now(),
        is_snapshot_update: false,
        content: read_existing_bytes(&path)?,
    };
    let snap = FileHistorySnapshot {
        message_id: ctx.current_message_id(),    // see §3
        tracked_file_backups: BTreeMap::from([(
            relative_path.to_string_lossy().into(),
            backup,
        )]),
    };
    ctx.session.append_file_history_snapshot(&snap, false)?;
}
```

### 3. `message_id` source

`FileHistorySnapshot` carries a `message_id` field (per TS
`utils/fileHistory.ts:41`). In TS the id is the assistant turn's
message id, threaded down through the tool runtime.

Rust doesn't have a clean `message_id` plumbing today. Two options:

1. **Store the current turn id on `Session`**: add
   `Session::current_turn_id: Arc<RwLock<Option<String>>>` and
   `set_current_turn/clear_current_turn`; cc-query calls `set` when
   it starts a turn and `clear` on turn end. Tools read from
   `ctx.session.current_turn_id.read().await`.
2. **Thread an Option<String> via ToolContext**: cc-query sets
   `ctx.message_id = Some(assistant_msg_id)` just-in-time before
   dispatch.

Option 2 is simpler. Add `pub message_id: Option<String>` to
`ToolContext`. When absent (e.g., in isolated tool tests), snapshot
with an empty string or skip the snapshot entirely; document that
the empty-string case is for tests only.

### 4. Regression tests

In `cc-query`:

- `engine::tests::cancel_during_tool_use_appends_canonical_markers` —
  harness cancels mid-stream when a tool_use block has arrived;
  assert the session JSONL now contains (a) an assistant-content
  marker string matching `INTERRUPT_MESSAGE`, (b) a synthetic
  tool_result using `INTERRUPT_MESSAGE_FOR_TOOL_USE`, (c) a
  standalone interrupt marker entry visible to the session reader.

- `engine::tests::cancel_without_tool_use_appends_plain_marker` —
  cancel fires with no dangling tool_use; assert only the plain
  marker is appended (not the tool-use variant).

In `cc-tools`:

- `edit::tests::successful_edit_appends_file_history_snapshot` —
  pre-existing file, Edit succeeds, session reader shows one
  `FileHistorySnapshot` with `is_snapshot_update = false` and the
  original bytes.
- `edit::tests::edit_on_missing_file_does_not_snapshot` — net-new
  file creation (Edit should error anyway; keep the guard).
- `write::tests::write_overwrite_appends_snapshot` — Write on an
  existing file snapshots pre-write contents.
- `write::tests::write_new_file_does_not_snapshot` — net-new file
  path → no snapshot (matches TS).

In `cc-session`: no new tests — existing coverage already locks
down the read/write shape.

### 5. Non-changes

- `cc-tui/src/render.rs:609` ("esc to interrupt · ctrl+c to cancel")
  stays as-is. That's the streaming-mode help footer, not a resume
  marker.
- `Session::append` paths and JSONL durability rules (§3 contract in
  `.claude/plan/implementation-notes.md`) are untouched.

## Impact

- **Affected specs**: `session-persistence` (extends the interrupt
  marker and file-history scenarios with "written by the cancel
  path / by the Edit/Write tools on success"). Depending on which
  tool-context option lands, a new `tool-context` cap may also be
  warranted.
- **Affected crates**:
  - `cc-query` — cancel path wiring + tests.
  - `cc-tools` — Edit + Write snapshot emission.
  - `cc-core` — new `ToolContext` struct + breaking change to
    `Tool::execute` (Option A).
  - All tools in `cc-tools/src/*` — signature migration (Option A).
  - `cc` (claude binary) — update tool dispatch site to pass
    `ToolContext`.
- **Compatibility**: `Tool::execute` breaking change is internal
  (no stable public API consumers yet). The JSONL wire format is
  unchanged — snapshots use the `file-history-snapshot` meta entry
  already parseable by `SessionEntry`.

## Open questions

1. If `ToolContext` feels too aggressive for one use case, scope
   down to Option B for this change and come back for A in the
   structured-Edit/Write output batch (P1 #32). Fine either way.
2. How big can a `FileHistoryBackup.content` be before the JSONL
   line is unreasonable? TS doesn't cap it. Propose a soft warn
   threshold of 1 MiB per backup; above that, log `tracing::warn!`
   and keep going (matches TS ship-anyway behaviour). Not gating.
3. `is_snapshot_update: bool` — TS uses `false` for the first
   backup of a file in a turn and `true` for subsequent edits of
   the same file in the same turn. Threading that requires the
   session to know which files were already snapshotted this turn.
   Lightweight path: expose
   `Session::has_snapshot_for_path_in_turn(&relpath) -> bool`
   backed by a `HashSet<String>` cleared by cc-query on turn
   boundaries. Propose landing this piece in a second commit if
   the first one is getting too big.
