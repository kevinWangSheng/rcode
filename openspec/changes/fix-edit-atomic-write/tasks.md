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
      QA 2026-04-21: reopened. The current implementation snapshots
      metadata in a second syscall after `read_to_string`, so the read
      content and the `(len, mtime)` snapshot are not tied to the same
      file view. A concurrent writer that lands between those two calls
      can still be silently clobbered.
      Fixed 2026-04-21 (round 2): snapshot now happens as a *pair* —
      one stat before `read_to_string` and one right after. The read
      is only accepted as "tied to a specific on-disk version" when
      the two snapshots match. When they differ we surface the
      lost-update error immediately instead of persisting a partial
      merge, which closes the race window the QA flagged (a write
      landing between the read and the old after-only stat would be
      absorbed silently).
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
      QA 2026-04-21: reopened with 2.1. The second pre-persist stat only
      works if the first snapshot corresponds to the same version of the
      file that was actually read; today that invariant is false.
      Fixed 2026-04-21 (round 2): the pre-persist stat now compares
      against the stat-pair-validated `read_snapshot` from §2.1, so the
      invariant holds. Two independent race windows are now policed:
      (a) writes during the read (stat-before vs. stat-after), and
      (b) writes during the compute-new-content window (stat-after vs.
      pre-persist stat). Either mismatch short-circuits with the
      lost-update error.

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
      Strengthened 2026-04-21 (round 2): the two §2 tests now prove the
      contract end-to-end. Added a `#[cfg(test)]`
      `TEST_PAUSE_AFTER_READ_MS` atomic that forces a deterministic
      gap between the Edit's read and its stat-after, so a concurrent
      writer is guaranteed to land in that window.
      `edit_detects_concurrent_write_between_stat_pair` (renamed from
      the old read-and-persist sanity check) drives one Edit + one
      parallel writer and hard-asserts (i) the Edit returns the
      "file changed on disk" error and (ii) the background writer's
      content is what ends up on disk — the Edit MUST NOT persist.
      `edit_lost_update_surfaces_clear_error` now hard-asserts
      `saw_lost_update` (previously `let _ = saw_lost_update`), looping
      up to 5 iterations with the same hook-driven deterministic race.
      An RAII `PauseGuard` resets the atomic on drop, so a failed
      assertion cannot slow down unrelated tests.
- [x] 3.2 Concurrency test: two threads each do an Edit on the same file;
      at least one edit lands, nothing is truncated.
- [x] 3.3 Mode preservation: pre-chmod the file to `0o755`, run Edit,
      verify mode survives.

## 4. Sign-off

- [x] 4.1 `cargo test -p cc-tools` + clippy clean.
      Re-verified 2026-04-21 (round 2): all cc-tools unit tests pass
      including the two hardened §2 tests; full workspace
      `cargo test --workspace` and `cargo clippy --workspace
      --all-targets -- -D warnings` are green.

## QA Notes

- 2026-04-21 validation reopened §2. The two new lost-update tests do
  not yet prove the contract:
  `edit_detects_lost_update_between_read_and_persist` verifies snapshot
  values can differ across two independent writes, but does not create a
  concurrent write inside one `EditTool::execute` call; and
  `edit_lost_update_surfaces_clear_error` explicitly does not assert
  that the lost-update error is actually observed. The atomic-write /
  truncation guarantees still look good; the conflict-detection
  guarantee does not.
- 2026-04-21 round-2 validation: §2 now upheld end-to-end. The
  stat-before + stat-after pair ties the read content to a single
  on-disk version, and both new tests drive a concurrent writer
  deterministically (via the `TEST_PAUSE_AFTER_READ_MS` hook) to
  prove the error is surfaced and the writer's content survives.
