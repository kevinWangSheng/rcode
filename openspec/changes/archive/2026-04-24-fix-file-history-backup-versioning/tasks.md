> **Depends on**: `fix-file-history-snapshot-producers` (merged
> `cd3b9b5`). No upstream blockers.

## 1. cc-session: per-path version selection

- [x] 1.1 Add a `next_backup_version(backup_dir: &Path, hex_prefix:
      &str) -> CcResult<u32>` free function in
      `rust/crates/cc-session/src/lib.rs`. Scans `backup_dir` for
      entries matching `{hex_prefix}@v{n}` and returns
      `max(n) + 1` (or `1` on empty / `NotFound`).
- [x] 1.2 In `Session::append_file_history_snapshot_for_path`,
      replace the hard-coded `format!("{hex}@v1")` with a call to
      `next_backup_version` and embed the chosen version in both
      the on-disk `backup_file_name` and the emitted
      `FileHistoryBackup.version`.
- [x] 1.3 Ordering invariant: `create_dir_all(&backup_dir)` MUST
      run before `next_backup_version` so the scan sees a valid
      directory. The existing code already mkdirs before the
      sidecar write; move the scan to that point.
- [x] 1.4 Entries in `backup_dir` that do not match the
      `{hex}@v{n}` shape (e.g. a stray `.DS_Store`, a partial
      `NamedTempFile` suffix) MUST be ignored — the helper uses
      `strip_prefix` + `parse::<u32>()` and skips on either
      failure. Include a unit test covering a dir with a
      `.tmpXXXX` leftover + a valid `@v3` to prove the scan
      tolerates noise.

## 2. cc-tools: regression tests

- [x] 2.1
      `cc-tools::edit::tests::two_edits_same_file_persist_distinct_backups`:
      seed `/tmp/.../x.rs = "a"`; run Edit `"a" → "b"`; run Edit
      `"b" → "c"`. Assert:
      (a) `session.file_history_snapshots()` returns exactly 2
          snapshots in append order;
      (b) the two `backup_file_name` values are distinct and end
          in `@v1` and `@v2`;
      (c) `session.read_backup("{hex}@v1") == b"a"` and
          `session.read_backup("{hex}@v2") == b"b"`;
      (d) the file on disk ends up as `"c"`.
- [x] 2.2
      `cc-tools::write::tests::two_writes_same_file_persist_distinct_backups`:
      same shape against Write. Seed `"a"`, write `"b"`, write
      `"c"`; assert two distinct sidecars holding `"a"` and
      `"b"`.
- [x] 2.3 Both tests share the existing `ctx_with_real_session`
      helper — no new test infra needed. Ensure the session ctx
      is reused across both tool calls so the snapshots land in
      the same JSONL.

## 3. cc-session: unit-level regression

- [x] 3.1
      `cc-session::tests::append_file_history_snapshot_for_path_versions_monotonically`:
      call the sink twice with the same relpath and distinct
      `prior_bytes`. Assert both sidecars exist on disk, names
      end in `@v1` / `@v2`, and the two reads return the bytes
      in the order they were written.
- [x] 3.2
      `cc-session::tests::next_backup_version_ignores_unrelated_entries`:
      seed `backup_dir` with `abc@v3`, `def@v9`, `.DS_Store`,
      `abc@v1.tmpABCD` (temp leftover). Assert
      `next_backup_version(dir, "abc")` returns `4` (only
      `abc@v3` counts).
- [x] 3.3
      `cc-session::tests::next_backup_version_returns_one_for_missing_dir`:
      a path that does not exist must yield `Ok(1)`, not an
      error.

## 4. Parity-roadmap bookkeeping

- [x] 4.1 In `.claude/plan/parity-gaps-2026-04-23.md`, replace the
      P0 #6 "⚠️ repeat-edit clobbers v1 sidecar — see follow-up"
      note with a resolved-pointer once this change lands.
      *The parent `fix-file-history-snapshot-producers` roadmap row
      pre-dated the QA finding, so no "⚠️" note was actually
      committed. Instead updated the P0 #6 row in-place to reflect
      `@v{n}` sidecar naming and cite this change by name.*

## 5. Verification

- [x] 5.1 `cargo fmt --all` clean.
- [x] 5.2 `cargo clippy --workspace --all-targets -- -D warnings`
      clean.
- [x] 5.3 `cargo test -p cc-session -p cc-tools` — the 5 new
      tests (3 in cc-session, 2 in cc-tools) pass alongside the
      existing suite.
- [~] 5.4 Manual sanity: tail the session JSONL after two Edits
      of the same file under a real run, confirm the two
      `file-history-snapshot` entries reference distinct
      `backupFileName` values and both sidecars are on disk with
      the expected bytes.
      *Deferred (needs a live API key + interactive run); the 5
      new regression tests prove the invariant programmatically.
      Operator can run this post-merge when convenient.*

## 6. Sign-off

- [x] 6.1 Commit message:
      "cc-session: per-path versioning for file-history sidecars
      (fix-file-history-backup-versioning)".
- [x] 6.2 Flip the P0 #6 ⚠️ footnote in
      `.claude/plan/parity-gaps-2026-04-23.md` to resolved.
- [x] 6.3 Post-merge, archive via
      `npx @fission-ai/openspec archive fix-file-history-backup-versioning`.
      *Archived 2026-04-24. Proposal's spec delta was
      `## MODIFIED` — flipped to `## ADDED` at archive time
      because the parent spec's requirement headers don't line up
      (the backup-versioning invariant is a net-new addition on
      top of the parent's "emit a snapshot" contract, not a
      modification of it).*
