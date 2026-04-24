# Proposal — Edit and Write tools emit FileHistorySnapshot on success

## Why

`fix-session-resume-integrity` shipped
`Session::append_file_history_snapshot` (+ `FileHistorySnapshot` /
`FileHistoryBackup` types). The 2026-04-23 QA confirmed the type +
persistence layer are solid, but also that **zero production code
calls the method**. Edit and Write in `rust/crates/cc-tools/`
mutate files without persisting pre-mutation bytes, so resuming a
session after an edit loses state that TS would have preserved via
a `file-history-snapshot` JSONL entry (TS reference:
`src/utils/fileHistory.ts:39-52`).

This is the last piece needed to close parity gap **P0 #6**
end-to-end. `fix-tool-context-refactor` gave tools the session
handle they need; this change uses it.

Also lands the cancel-path regression tests for the interrupt-
marker wiring that `fix-session-resume-wiring` §1 already put in
place. The tests got deferred for the same reason as
`fix-hook-correctness-wiring` §5.3 — the engine had no way to drive
a scripted stream. `fix-engine-stream-mock-harness` fixes that, so
we can land the tests here alongside the feature that they'd
complement.

## Goal

On every **successful** Edit or Write against a **pre-existing**
file:

1. Capture the file's current bytes.
2. Wrap them in a `FileHistoryBackup { backup_file_name, backup_time,
   is_snapshot_update: false, content }`.
3. Wrap that in a `FileHistorySnapshot { message_id:
   ctx.message_id.unwrap_or_default(), tracked_file_backups }`.
4. Call `ctx.session.append_file_history_snapshot(&snap, false)`.
5. Continue with the write.

Skip the snapshot on:

- Net-new file creation (path did not exist pre-mutation) — matches
  TS.
- Any failed mutation — no snapshot on validation error, lost-
  update detection, cancel, permission denial.
- Tool calls without a session context (isolated tool tests using
  `ToolContext::for_test_bare` get a noop sink — the call
  succeeds but writes nothing).

Not in scope:

- `is_snapshot_update: true` differentiation across repeated edits
  in one turn. That piece (per-turn relpath tracking) is a polish
  pass — leave `is_snapshot_update` hard-coded `false` and file a
  follow-up if replay granularity becomes a concern.
- WebEdit / SedEdit tools — Rust doesn't ship those yet.
- LSP diagnostic clearing / skill activation on touched paths
  (P2 #44 items; tracked separately).

## What changes

### 1. Write tool

```rust
// rust/crates/cc-tools/src/write.rs, in the execute() body,
// AFTER path resolution (relpath in hand) and BEFORE the existing
// atomic write:

if path.exists() {
    // Soft size cap to avoid pathological JSONL blowups.
    let prior = std::fs::read(&path)
        .map_err(|e| CcError::Other(format!(
            "write: failed to snapshot pre-write contents: {e}"
        )))?;
    if prior.len() > MAX_SNAPSHOT_BYTES {
        tracing::warn!(
            path = %relpath,
            size = prior.len(),
            "file-history snapshot exceeds {MAX_SNAPSHOT_BYTES} bytes; persisting anyway"
        );
    }
    let backup = cc_core::FileHistoryBackup {
        backup_file_name: relpath.clone(),
        backup_time: std::time::SystemTime::now(),
        is_snapshot_update: false,
        content: prior,
    };
    let snap = cc_core::FileHistorySnapshot {
        message_id: ctx.message_id.clone().unwrap_or_default(),
        tracked_file_backups: std::collections::BTreeMap::from([
            (relpath.clone(), backup),
        ]),
    };
    ctx.session.append_file_history_snapshot(&snap, false)?;
}

// Existing atomic-write path follows unchanged.
```

Constants: pick `MAX_SNAPSHOT_BYTES = 64 * 1024 * 1024` (64 MiB).
Soft — log a warning and continue. Tight enough to catch 500 MiB
accidental writes; loose enough to handle any real-world source
file.

### 2. Edit tool

```rust
// rust/crates/cc-tools/src/edit.rs, at the existing "first stat"
// precondition check (after verifying the file exists and after
// computing the `(len, mtime)` snapshot used for lost-update
// detection, before the `old_string → new_string` replacement is
// applied):

let backup = cc_core::FileHistoryBackup {
    backup_file_name: relpath.clone(),
    backup_time: std::time::SystemTime::now(),
    is_snapshot_update: false,
    content: original_bytes.clone(), // bytes already read for diffing
};
let snap = cc_core::FileHistorySnapshot {
    message_id: ctx.message_id.clone().unwrap_or_default(),
    tracked_file_backups: std::collections::BTreeMap::from([
        (relpath.clone(), backup),
    ]),
};
ctx.session.append_file_history_snapshot(&snap, false)?;
```

Important: `original_bytes` here is the SAME bytes Edit already
reads for stat-pair lost-update detection. Don't re-read the file —
reuse the existing in-memory copy so snapshot bytes are guaranteed
byte-identical to what the model's replacement was computed
against.

Edit must guarantee the snapshot is emitted ONLY when the
replacement itself will succeed. Place the append after every
precondition check (file exists, old_string found, uniqueness
check, stat-pair match) and before the actual write. On failure
of any check, return early **without** appending the snapshot.

### 3. Cancel-path regression tests (using the stream-mock harness)

Use `fix-engine-stream-mock-harness`'s `scripted_stream` helper in
`cc-query/src/engine.rs::tests`:

- **`cancel_during_tool_use_appends_canonical_markers`** — drive a
  script that yields `MessageStart` + one `tool_use` block + cancel;
  assert the session JSONL contains (a) an assistant-content
  message ending in `INTERRUPT_MESSAGE`, (b) a synthetic
  tool_result message whose content equals
  `INTERRUPT_MESSAGE_FOR_TOOL_USE`, (c) a standalone canonical
  marker entry produced by
  `Session::append_interrupt_marker(true)`.

- **`cancel_without_tool_use_appends_plain_marker`** — script
  yields `MessageStart` + one text delta + cancel (no tool_use).
  Assert (a) is present; (b) no synthetic tool_result; (c)
  standalone marker is the plain variant,
  `append_interrupt_marker(false)`.

Both tests live next to the existing `drain_stream` / PreToolUse
tests in engine.rs.

### 4. Tool-level regression tests

In `cc-tools/src/write.rs::tests`:

- **`write_overwrite_appends_snapshot`** — write a temp file with
  `"old"`; call `WriteTool::execute({"file_path": <path>,
  "content": "new"})` with a ctx whose session is a real
  `Session` in a temp dir; after the call, confirm the file
  contains `"new"` and that
  `Session::file_history_snapshots()` returns exactly one
  snapshot whose single backup has `content == b"old"` and
  `is_snapshot_update == false`.
- **`write_new_file_does_not_snapshot`** — target path did not
  exist; Write succeeds; `file_history_snapshots()` returns an
  empty vec.

In `cc-tools/src/edit.rs::tests`:

- **`successful_edit_appends_file_history_snapshot`** — temp file
  `"fn a() {}\nfn b() {}\n"`; Edit replaces `"fn a"` → `"fn x"`;
  after the call, file is `"fn x() {}\nfn b() {}\n"` and
  snapshot vec contains exactly one entry with pre-edit bytes and
  `is_snapshot_update == false`.
- **`failed_edit_does_not_snapshot`** — temp file `"abc"`; Edit
  with `old_string = "zzz"` fails; snapshot vec is empty; file on
  disk is unchanged.

Reuse the existing Edit / Write test harness patterns; the only
novelty is constructing a `ToolContext` that holds a real
`Session` (rather than a noop sink) so the append actually
persists to the JSONL file the test reads back.

### 5. Parity-roadmap bookkeeping

After this change lands, P0 #5 (interrupt marker) and P0 #6 (file-
history snapshot) flip from "partial — types shipped, consumer
wiring pending" to "end-to-end live" in
`.claude/plan/parity-gaps-2026-04-23.md`.

## Impact

- **Affected specs**: new `file-history-snapshot-producers`
  capability.
- **Affected crates**:
  - `cc-tools` — Edit + Write produce snapshots on success.
  - `cc-query` — two new cancel-path regression tests (no
    production change).
- **Compatibility**: additive. Existing Edit / Write callers see
  the same inputs and the same outputs; the session gains JSONL
  entries that a resume-reading path already understands
  (`SessionEntry::Meta(MetaEntry::FileHistorySnapshot { … })` is
  already parseable via the union landed in
  `fix-session-resume-integrity`).
- **Wire format**: no change.
- **Performance**: one extra file read per successful Edit/Write
  — dominated by the OS page cache for files we just stat'd.
  Soft-warn over 64 MiB.

## Open questions

1. `MAX_SNAPSHOT_BYTES` soft cap: 64 MiB feels right for source
   code, tight enough to flag accidental binary writes. Tune if
   real workloads show pathological hits.
2. Should Edit failures on the stat-pair lost-update path skip
   the snapshot? Current proposal: yes — lost-update is a failure
   mode, no snapshot. Matches TS.
3. The snapshot append happens after the precondition checks but
   before the actual write. If the write itself fails (disk full,
   permission denied mid-write), we end up with a snapshot of
   pre-write contents but no actual change. Acceptable — the
   replay reader handles "orphan snapshots" (a snapshot that
   doesn't correspond to a subsequent write) by re-applying the
   saved content, which is a no-op since disk already has that
   content. TS has the same ordering.
