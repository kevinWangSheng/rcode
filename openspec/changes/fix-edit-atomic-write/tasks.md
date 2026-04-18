## 1. Atomic Write

- [x] 1.1 Add `tempfile` to `cc-tools/Cargo.toml` runtime deps if not
      already present.
- [x] 1.2 In `EditTool::execute`, after computing `new_content`, create
      a `NamedTempFile::new_in(parent)`, write + flush + sync_all, then
      `persist(path)`.
- [x] 1.3 Preserve the pre-existing file mode on the tempfile before
      persist.

## 2. Lost-Update Detection (optional but recommended)

- [x] 2.1 Capture `(len, mtime)` snapshot at read time.
      Fixed 2026-04-18: `EditTool::execute` now calls a new
      `snapshot_metadata(path)` helper immediately after the
      `read_to_string`, capturing `(len, Option<SystemTime>)` via
      `tokio::fs::metadata` + `Metadata::modified()`. See
      `cc-tools/src/edit.rs`.
- [x] 2.2 Before persist, stat the path again; if the snapshot no longer
      matches, return a `ToolResult::error` asking the caller to re-read.
      Fixed 2026-04-18: added post-compute `snapshot_metadata` stat and
      mismatch short-circuit with message `"file changed on disk since
      last read; call Read again before Edit"`. Chose option (b) from
      the proposal — auto-snapshot within a single `execute` — because
      threading a caller-provided snapshot would require contract
      changes to every Edit caller in cc-query and cc-agents. The
      auto-snapshot catches the concurrency race (two Edits overlapping
      within the same engine tick) which is the hot failure mode.

## 3. Tests

- [x] 3.1 Crash test: spawn a child that starts an Edit, SIGKILL mid-way,
      reopen; file is either the old version or the new version, never
      truncated.
      Fixed 2026-04-18: added `edit_sigkill_mid_persist_never_truncates_file`
      (parent) + `crash_child_entry_point` (child). Parent re-execs the
      test binary via `std::process::Command`, gated on the
      `CC_EDIT_CRASH_CHILD` env var. Child writes the pre-image, opens
      a sibling `NamedTempFile`, writes + flushes + syncs the new
      content, `mem::forget`s the tempfile, then `libc::_exit(9)` —
      matching the "SIGKILL between sync and rename" window. Parent
      asserts the target is never truncated and matches either the
      pre-image or post-image exactly.
      Also added `edit_detects_lost_update_between_read_and_persist` and
      `edit_lost_update_surfaces_clear_error` for §2 coverage.
- [x] 3.2 Concurrency test: two threads each do an Edit on the same file;
      at least one edit lands, nothing is truncated.
- [x] 3.3 Mode preservation: pre-chmod the file to `0o755`, run Edit,
      verify mode survives.

## 4. Sign-off

- [x] 4.1 `cargo test -p cc-tools` + clippy clean.
