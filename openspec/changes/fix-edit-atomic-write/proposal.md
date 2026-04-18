## Why

`cc-tools/src/edit.rs` performs an edit as three separate syscalls:

1. `tokio::fs::read_to_string(path)`
2. in-memory string replace
3. `tokio::fs::write(path, new_content)`

Nothing between step 1 and step 3 is atomic. Two consequences:

- **Truncation window.** `tokio::fs::write` internally opens with
  `truncate(true) + create(true)`. If the process is killed between the
  open and the full write, the file is now empty or partial. A naive
  reader (including Claude itself on the next turn) sees a file that
  lost all content.
- **Lost updates under concurrency.** Two Edit calls racing on the same
  file both read version N, both produce version N+1, both write. The
  second writer wins; the first edit silently disappears. Nothing in
  the current implementation detects or reports this.

Neither failure mode is hypothetical — a single user running a multi-
agent workflow (local_agent, in_process_teammate) can trigger the
concurrency race, and any SIGKILL during Edit triggers truncation.

## What Changes

- Switch `EditTool::execute` to the atomic tempfile + rename pattern:
  1. open `tempfile::NamedTempFile::new_in(parent_of(path))` so the temp
     file lands on the same filesystem (rename must be same-fs).
  2. write the new content into the temp file, `flush` + `sync_all`.
  3. `persist(path)` which does `rename(tmp, path)` — atomic on POSIX.
- Preserve the original file mode: `metadata(path).permissions()` before
  the write, apply with `set_permissions` on the tempfile before persist.
- On concurrent Edits: the rename is atomic, so at least one full edit
  lands. A follow-up (below) defines best-effort conflict detection.
- Optional: detect "lost update" by comparing the post-read `mtime` +
  `len` against a pre-read snapshot; if changed, return a
  `ToolResult::error` so Claude can re-read and retry.

## Capabilities

### Modified Capabilities
- `tool-edit-atomic`: edits MUST be durable via tempfile + rename, and
  MUST preserve file mode.

## Impact

- **Affected code:** `cc-tools/src/edit.rs`. `Write` has a closely
  related problem tracked separately (`fix-write-preserve-mode`) and
  should use the same pattern when it's changed.
- **Deps:** `tempfile` crate (likely already in the tree via dev-deps;
  promote to runtime if not).
- **Risk:** LOW.
