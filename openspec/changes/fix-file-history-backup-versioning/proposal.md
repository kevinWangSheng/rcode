# Proposal — File-history backup sidecars use per-path version numbers

## Why

`fix-file-history-snapshot-producers` (merge `cd3b9b5`) wired Edit
and Write to call
`SessionSink::append_file_history_snapshot_for_path`. The
cc-session implementation hashes the relpath and writes the pre-
mutation bytes to `{session_dir}/file-history/{sha256(relpath)[0..16]}@v1`.
The `@v1` suffix is hard-coded and `tempfile::NamedTempFile::persist`
is overwrite-on-conflict, so a second successful Edit of the same
path in the same session does this:

1. `Edit #1` reads `"content-v1"`, persists sidecar
   `abc0123456789abc@v1` = `"content-v1"`, appends snapshot entry
   referencing `abc0123456789abc@v1`, rewrites file to
   `"content-v2"`.
2. `Edit #2` reads `"content-v2"`, persists sidecar
   `abc0123456789abc@v1` (same name — **overwrites**) =
   `"content-v2"`, appends a second snapshot entry referencing the
   same `abc0123456789abc@v1`, rewrites file to `"content-v3"`.

The JSONL now has two distinct `FileHistorySnapshot` entries, both
pointing at `abc0123456789abc@v1`, which on disk contains
`"content-v2"`. A replay tool asked to rewind to snapshot #1 reads
the sidecar and gets `"content-v2"`, silently restoring the wrong
bytes.

The TS reference explicitly documents this race and avoids it at a
different layer — `fileHistoryTrackEdit` (`src/utils/fileHistory.ts:99-118`)
bails out if the path is already tracked in the current turn:

```ts
// Speculative writes would overwrite the deterministic {hash}@v1 backup on every
// repeat call — a second trackEdit after an edit would corrupt v1 with post-edit
// content.
if (mostRecent.trackedFileBackups[trackingPath]) {
  return
}
```

The parent change's §5 deferral (per-turn `is_snapshot_update`
tracking is out of scope) means Rust doesn't have the turn-boundary
state the TS guard needs. So the fix here operates at the
**sidecar-naming** layer instead: bump the version so every
snapshot points at a distinct file. This is wire-compatible with
TS (TS also emits `@v2`, `@v3`, … from `fileHistoryMakeSnapshot`),
preserves every pre-edit state (strictly stronger than TS which
only keeps the first-of-turn), and needs no turn coordination.

Surfaced during QA of `fix-file-history-snapshot-producers` on
2026-04-23. Not caught by that change's regression tests because
none of them edits the same file twice.

## Goal

Two successful Edits (or Write + Edit, or two Writes) against the
same path in one session MUST produce two distinct sidecar files,
each preserving the exact pre-mutation bytes its snapshot entry
points at. Replay/rewind to either snapshot MUST restore the
correct bytes.

Out of scope:

- Per-turn `is_snapshot_update = true` differentiation. Still
  deferred; all producer-path snapshots continue to emit with
  `is_snapshot_update = false`.
- Pruning of old backups. A long session editing the same file
  many times accumulates `@v1 … @v{n}` sidecars. That's cheap to
  leave for now; file a separate change if real workloads show
  disk-usage pressure.
- Cross-session dedup. Each session keeps its own
  `{session_dir}/file-history/` tree; versions are per-session.

## What changes

### 1. `cc-session::Session::append_file_history_snapshot_for_path`

Replace the hard-coded `@v1` with a version chosen by scanning the
backup directory for existing `{hex}@v{n}` entries and picking
`max(n) + 1` (or `1` if none). Disk is the source of truth so
no in-memory state is needed; this sidesteps the `Session: Clone`
and `Arc<dyn SessionSink>` state-sharing question entirely.

```rust
// In append_file_history_snapshot_for_path, replace:
-    let backup_file_name = format!("{hex}@v1");
-    let backup_path = backup_dir.join(&backup_file_name);
+    let version = next_backup_version(&backup_dir, &hex)?;
+    let backup_file_name = format!("{hex}@v{version}");
+    let backup_path = backup_dir.join(&backup_file_name);
```

And the `FileHistoryBackup` built below now carries the actual
`version` instead of a hard-coded `1`.

New free function in the same module:

```rust
fn next_backup_version(backup_dir: &Path, hex_prefix: &str) -> CcResult<u32> {
    // Scan the backup dir for entries matching `{hex_prefix}@v{n}` and return
    // max(n) + 1. Returns 1 if the directory is empty or missing (the caller
    // creates the dir before this helper runs, so "missing" only happens on
    // a deleted-between-mkdir-and-scan race — still safe to treat as empty).
    let read_dir = match fs::read_dir(backup_dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(1),
        Err(e) => return Err(CcError::io(format!(
            "failed to scan backup dir {}: {e}", backup_dir.display()
        ))),
    };

    let mut max_seen: u32 = 0;
    let prefix = format!("{hex_prefix}@v");
    for entry in read_dir.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(rest) = name.strip_prefix(&prefix) else { continue };
        if let Ok(n) = rest.parse::<u32>() {
            if n > max_seen {
                max_seen = n;
            }
        }
    }
    Ok(max_seen + 1)
}
```

Ordering: mkdir → scan → choose `n` → write tmp → persist. The
scan-then-persist pair is not atomic across processes (two
concurrent tool dispatches could both see `n=1` and race on `@v2`),
but cc-query serialises mutating-tool dispatch today so this cannot
happen in-process. Cross-process races (two CLIs sharing a session
dir) would clobber one version; that's an edge case we acknowledge
rather than solve here.

### 2. Tool call sites unchanged

`cc-tools::write.rs` and `cc-tools::edit.rs` already go through
`ctx.session.append_file_history_snapshot_for_path(...)` — no
change needed at the call sites. Version selection is an
implementation detail of the sink.

### 3. Regression tests (the gap that missed this originally)

Add one test per tool to lock in the invariant:

- `cc-tools::edit::tests::two_edits_same_file_persist_distinct_backups`
  — seed `/proj/x.rs` = `"a"`, edit `"a" → "b"`, edit `"b" → "c"`.
  After both edits: exactly 2 snapshot entries, each with a
  distinct `backup_file_name`, and the sidecars contain `"a"` and
  `"b"` respectively (in snapshot order).

- `cc-tools::write::tests::two_writes_same_file_persist_distinct_backups`
  — seed `/proj/x.txt` = `"a"`, write `"b"`, write `"c"`. Same
  assertion shape.

- `cc-session::tests::append_file_history_snapshot_for_path_versions_monotonically`
  — call the sink twice for the same relpath with distinct bytes;
  assert both sidecars exist on disk, with distinct names, and
  `session.file_history_snapshots()` returns them in order each
  pointing at its own sidecar.

### 4. Parity-roadmap bookkeeping

Update `.claude/plan/parity-gaps-2026-04-23.md` P0 #6 footnote:
replace the "⚠️ repeat-edit corrupts v1 sidecar — see follow-up"
note (added by this change's merge of QA findings) with a
resolved marker once this change lands.

## Impact

- **Affected specs**: `file-history-snapshot-producers` (MODIFIED —
  the version-increment invariant joins the existing producer
  contract).
- **Affected crates**:
  - `cc-session` — `append_file_history_snapshot_for_path`
    delegates naming to the new `next_backup_version` helper.
  - `cc-tools` — call sites unchanged; two new regression tests.
- **Compatibility**: wire-compatible with TS (`@v{n}` is the exact
  format TS uses). Backward-compatible with snapshots written by
  the parent change: those reference `{hex}@v1` and the scan will
  find them and assign `@v2` to the next Edit, so mixed-version
  sessions resume correctly.
- **Performance**: one `read_dir` per successful Edit/Write. For
  a session with K prior backups the scan is O(K). Sessions with
  pathological backup counts would already be hitting other
  concerns (disk usage); this is not the bottleneck.

## Open questions

1. Should the scan cache the next version in-memory across calls?
   **Current answer: no.** A `Mutex<BTreeMap<hex, u32>>` on
   `Session` would avoid the `read_dir`, but Session is `Clone` +
   `Arc<dyn SessionSink>` and sharing the map across clones is
   non-trivial. The scan is cheap on the hot path (a handful of
   entries, page-cache-hot). Revisit if profiling shows it.
2. Pruning of `@v{n}` for small `n` after a `MakeSnapshot` turn
   boundary: deferred with per-turn tracking (parent §5).
3. Cross-process concurrent writers to the same session dir:
   explicitly out of scope — cc-query serialises mutating tools
   in-process, and two CLIs against one session is not supported
   elsewhere in the architecture either.
