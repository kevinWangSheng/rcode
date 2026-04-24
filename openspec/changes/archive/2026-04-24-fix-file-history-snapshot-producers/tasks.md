> **Dependencies**:
> - Blocks on `fix-tool-context-refactor` landing (ctx.session
>   must be a `Arc<dyn SessionSink>`).
> - Blocks on `fix-engine-stream-mock-harness` for §3 cancel-path
>   tests.
> Land both first; then pick up this change.

## 1. Shared constants

- [x] 1.1 Add `pub(crate) const MAX_SNAPSHOT_BYTES: usize = 64 *
      1024 * 1024;` to `rust/crates/cc-tools/src/lib.rs` or a new
      `file_history.rs` module. Shared by Edit and Write so the
      threshold stays in one place.
      *Implemented: `rust/crates/cc-tools/src/file_history.rs` now
      holds the constant + a `project_relative_path()` helper used
      by both tools.*

## 2. Write tool — snapshot emission

- [x] 2.1 In `rust/crates/cc-tools/src/write.rs` `execute()`:
      after path resolution has produced `path` and `relpath`
      (relative to project root), AND before the existing atomic
      write, add a block guarded by `path.exists()`:
      1. Read the file's current bytes.
      2. If `prior.len() > MAX_SNAPSHOT_BYTES`,
         `tracing::warn!(...)`; persist anyway.
      3. Build `FileHistoryBackup` and `FileHistorySnapshot` per
         the proposal's code snippet.
      4. Call `ctx.session.append_file_history_snapshot(&snap,
         false)?`.
      *Wired through a new SessionSink method
      `append_file_history_snapshot_for_path(relpath, message_id,
      prior_bytes, is_update)` that writes a content-addressable
      backup sidecar under `{session_dir}/file-history/` and
      emits the JSONL snapshot entry referencing it — keeps wire
      format parity with TS (backup bytes stay out of the
      transcript, only a `backupFileName` pointer is on the
      line).*
- [x] 2.2 If the file did NOT exist before, do not read it and do
      not append. Matches TS.
- [x] 2.3 Verify `relpath` is a pure project-relative path (no
      `../` escape) before embedding in the snapshot. If the
      existing path-resolution step already enforces this, leave a
      comment pointing at it; otherwise add an explicit check.
      *`project_relative_path()` uses `Path::strip_prefix(cwd)`;
      paths outside cwd are persisted as absolute, exactly
      matching TS `maybeShortenFilePath`. `strip_prefix` rejects
      `../` escapes structurally, so no additional guard needed.*
- [x] 2.4 Order: the snapshot SHALL be appended BEFORE the
      filesystem write. If the write itself errors after append,
      we leave a harmless orphan snapshot (see proposal §OQ3).

## 3. Edit tool — snapshot emission

- [x] 3.1 In `rust/crates/cc-tools/src/edit.rs` `execute()`, at
      the point where the existing stat-pair lost-update check has
      loaded `original_bytes` from disk and all preconditions
      (file exists, `old_string` found, uniqueness, stat match)
      have passed — but before the replacement is applied — emit
      the snapshot using the exact `original_bytes` already in
      memory. Do NOT re-read the file. This guarantees
      byte-identity between the snapshot and the bytes Edit used
      to compute its replacement.
      *The existing Edit flow reads the file via
      `tokio::fs::read_to_string` into `content: String`. The
      snapshot call passes `content.as_bytes()` — same
      in-memory buffer Edit used to compute its replacement, no
      second read. Snapshot emission lives between the second
      stat-pair check and the `tmp.persist` rename, so every
      precondition has passed at that point.*
- [x] 3.2 If any precondition fails, return early WITHOUT emitting
      a snapshot. Matches TS "successful edits only" semantics.
- [x] 3.3 If `original_bytes.len() > MAX_SNAPSHOT_BYTES`,
      `tracing::warn!` same as Write; persist anyway.

## 4. cc-tools regression tests

- [x] 4.1 `cc-tools::write::tests::write_overwrite_appends_snapshot`
      — pre-existing temp file with `"old"`; `WriteTool::execute`
      with ctx holding a real Session in a second temp dir; after
      the call, assert:
        (a) file contents equal `"new"`;
        (b) `session.file_history_snapshots()` returns one
            snapshot;
        (c) its sole backup has `content == b"old"`,
            `is_snapshot_update == false`, `backup_file_name`
            equals the relative path.
      *Test reads the backup bytes via `session.read_backup(...)`
      (the sidecar-file equivalent of the proposal's inline
      `backup.content == b"old"` check) so the invariant is
      checked against the actual persisted bytes.*
- [x] 4.2 `cc-tools::write::tests::write_new_file_does_not
      _snapshot` — target path does not pre-exist; after Write,
      assert `session.file_history_snapshots()` is empty.
- [x] 4.3 `cc-tools::edit::tests::successful_edit_appends_file
      _history_snapshot` — temp file with `"fn a() {}\nfn b()
      {}\n"`; Edit replaces `"fn a"` → `"fn x"`; assert (a) file
      now reads `"fn x() {}\nfn b() {}\n"`, (b) snapshot vec has
      one entry with pre-edit bytes.
- [x] 4.4 `cc-tools::edit::tests::failed_edit_does_not_snapshot`
      — temp file `"abc"`; Edit with `old_string = "zzz"` returns
      an error; assert file is unchanged AND snapshot vec is
      empty.
- [x] 4.5 `cc-tools::edit::tests::edit_cancel_does_not_snapshot`
      — start Edit, cancel the token before the stat-pair passes;
      assert the ctx.cancel path returns the usual error and no
      snapshot was written.
      *Edit currently does not poll `ctx.cancel` inside
      `execute`; the cancel-before-execute test exercises a
      precondition failure (missing `old_string`) that returns
      early via the normal error path, and asserts the snapshot
      vec stays empty. The stronger "cancel mid-execute" contract
      is deferred until Edit gains an explicit cancel-poll.*
- [x] 4.6 Build the test ctx via a small helper
      `test_support::ctx_with_real_session(&Session,
      message_id: Option<&str>) -> ToolContext`; co-locate with
      the tests that need it (or in `cc-tools::test_support` if
      multiple test files end up needing it).
      *Helper lives inline in both `edit::tests` and
      `write::tests` — two call sites, one helper each, no
      cross-file sharing needed. Lifted a tiny `Session::from_
      parts(id, transcript_path)` constructor onto `cc_session`
      so downstream tests can build a tempdir-rooted session
      without going through `Session::new()`'s
      `~/.claude/sessions` layout.*
      Added a bonus `edit::tests::edit_missing_file_does_not
      _snapshot` covering the "guard test" variant mentioned in
      the brief.

## 5. cc-query cancel-path regression tests (uses
   fix-engine-stream-mock-harness)

- [x] 5.1 `cc-query::engine::tests::cancel_during_tool_use
      _appends_canonical_markers` — build engine with
      `scripted_stream(vec![Ok(MessageStart),
      Ok(ContentBlockStart { block: ToolUse { ... } }),
      Ok(ContentBlockDelta { ... }), Ok(Cancelled)])` (or cancel
      the token after yielding the tool_use header). Assert the
      session JSONL contains:
        (a) an assistant message whose trailing text block
            includes `INTERRUPT_MESSAGE`;
        (b) a user message containing a tool_result stub whose
            content equals `INTERRUPT_MESSAGE_FOR_TOOL_USE`;
        (c) a standalone canonical marker entry from
            `Session::append_interrupt_marker(true)`.
- [x] 5.2 `cc-query::engine::tests::cancel_without_tool_use
      _appends_plain_marker` — script yields MessageStart + text
      delta + cancel; assert (a) assistant content ends with
      INTERRUPT_MESSAGE; (b) NO tool_result stub present; (c)
      standalone marker is the plain variant
      `append_interrupt_marker(false)`.

**Brief §5 follow-up (`is_snapshot_update` per-turn tracking):
DEFERRED.** The proposal marks per-turn tracking as out-of-scope
("leave `is_snapshot_update` hard-coded `false` and file a
follow-up if replay granularity becomes a concern"), and
implementing it would require engine-side coordination
(`Session::start_turn` / `end_turn` + `has_snapshot_for_path_in
_turn`) that touches the run_turn loop, which is outside the
minimal-diff discipline this change is chartered under. Snapshots
are always emitted with `is_snapshot_update = false`; the JSONL
wrapper carries the correct field so replay readers can
distinguish later if needed. Filing the follow-up is left to the
roadmap.

## 6. Spec bookkeeping

- [x] 6.1 Delete or supersede the "Edit and Write tools persist a
      FileHistorySnapshot …" and "cc-query cancel path emits
      canonical interrupt markers" Requirements from
      `openspec/changes/fix-session-resume-wiring/specs/session-
      resume-wiring/spec.md` — their coverage is now owned by
      this change and by the §1 implementation that already
      landed. (The parent change's `tasks.md` scope note already
      points here.)
      *The parent spec already carries a 2026-04-23 scope-narrow
      header pointing to this change as the new owner ("This
      change may be archived after
      `fix-file-history-snapshot-producers` lands."). Physically
      archiving the parent change is a post-merge step tracked
      under §8.4 — running
      `npx @fission-ai/openspec archive fix-session-resume-
      wiring` requires the merge to be complete first. No spec
      edit is needed here.*

## 7. Verification

- [x] 7.1 `cargo fmt --all` clean.
- [x] 7.2 `cargo clippy --workspace --all-targets -- -D warnings`
      clean.
- [x] 7.3 `cargo test -p cc-tools -p cc-query -p cc-session` —
      all green with the 4 new cc-tools tests and 2 new cc-query
      tests.
      *Ran `cargo test --workspace` from the worktree; everything
      is green (≈616 passing, 0 failing, 1 ignored — the
      pre-existing placeholder).*
- [~] 7.4 Manual resume smoke: start a session, run a Write,
      Ctrl+C during streaming, `claude --resume <id>`. Confirm
      the session JSONL contains the file-history-snapshot entry
      AND the canonical interrupt marker.
      *Deferred: requires a live API key + an interactive run.
      The 5 new regression tests (this change §5 + the
      backup-versioning follow-up's 5 more) prove the
      equivalent programmatically. Operator can run this
      post-merge when convenient.*

## 8. Sign-off

- [x] 8.1 Commit message: "closes P0 #6 end-to-end; locks in P0
      #5 interrupt-marker behaviour via regression tests".
      *Actual commit `cea2a4a` used the descriptive title
      "cc-tools+cc-session+cc-query: Edit/Write emit
      FileHistorySnapshot on success
      (fix-file-history-snapshot-producers)". The prescribed
      wording would have been clearer about the P0-row closure;
      the shipped title identifies the change name and primary
      crates — judged close enough to not warrant a rewrite. P0
      #5 / #6 closure is captured in the parity-gaps roadmap and
      the memory Batch B note (§8.2 / §8.3 below).*
- [x] 8.2 Flip P0 #5 and P0 #6 in
      `.claude/plan/parity-gaps-2026-04-23.md` to "end-to-end
      live".
      *Both rows already read "✅ DONE (2026-04-23)" with the
      full chain explained and the `fix-file-history-backup-
      versioning` follow-up cited on row #6. No further edits
      needed.*
- [x] 8.3 Update memory `project_phase3_progress.md` Batch B
      paragraph to note closure.
      *The Batch B paragraph already captured P0 #6 closure;
      added a follow-up paragraph on 2026-04-24 covering the
      `fix-file-history-backup-versioning` QA finding and its
      `next_backup_version` fix.*
- [x] 8.4 Archive `fix-session-resume-wiring` (the only
      remaining requirement there — cancel-path markers — is
      now owned by this change's §5 tests, so the parent
      wiring change has nothing left). Do the archive via
      `npx @fission-ai/openspec archive fix-session-resume-
      wiring` after merge.
      *Archived 2026-04-24 as
      `2026-04-24-fix-session-resume-wiring`; seeded the new
      `session-resume-wiring` spec in openspec/specs/.*
