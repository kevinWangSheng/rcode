> **Scope note (2026-04-23):** §1 has landed in commit on
> `phase3/implementation`. §2 (ToolContext refactor) plus §3/§4
> (Edit/Write file-history snapshots) and §5/§6/§7/§8 are deferred to
> a follow-up change because §2 is a breaking change to
> `Tool::execute` that cascades to ~28 tool implementations in
> `cc-tools/`. Splitting keeps the interrupt-marker fix landable now
> and isolates the wider trait migration.

## 1. Interrupt-marker wiring — cc-query cancel path

- [x] 1.1 In `rust/crates/cc-query/src/engine.rs` add
      `use cc_session::{INTERRUPT_MESSAGE, INTERRUPT_MESSAGE_FOR_TOOL_USE};`
      to the existing `use cc_session::…` block near the top.
- [x] 1.2 Replace the literal `"\n[Interrupted by user]"` at
      `engine.rs:224` with `format!("\n{}", INTERRUPT_MESSAGE)`.
- [x] 1.3 Replace the literal `"[Interrupted by user]"` at
      `engine.rs:246` with `INTERRUPT_MESSAGE_FOR_TOOL_USE.to_string()`.
- [x] 1.4 After the existing `self.session.append(&partial_msg)?`
      (line 260) and the conditional `self.session.append(&result_msg)?`
      (line 268), add:
      ```
      self.session.append_interrupt_marker(
          !interrupted_tool_results.is_empty(),
      )?;
      ```
      This writes the canonical resume-detection entry AFTER the
      content blocks are persisted. Order matters: the content
      blocks have to be on disk before the marker so a concurrent
      reader can't see marker-without-content.
- [x] 1.5 Do not touch `cc-tui/src/render.rs:609` — that is
      streaming-mode button help, unrelated.

## 2. Tool-context refactor (Option A)

- [ ] 2.1 Add `pub struct ToolContext` in `rust/crates/cc-core/src/tool.rs`
      with fields:
      ```
      pub session: Arc<cc_session::Session>,
      pub cancel: tokio_util::sync::CancellationToken,
      pub message_id: Option<String>,
      ```
      (Keeps the door open for more context fields like project_dir
      / settings later.)
- [ ] 2.2 Change the `Tool::execute` signature at `tool.rs:75` from
      `execute(&self, input: Value, cancel: &CancellationToken)` to
      `execute(&self, input: Value, ctx: &ToolContext)`. Keep
      `CcResult<ToolResult>` return type.
- [ ] 2.3 Migrate every tool in `rust/crates/cc-tools/src/*.rs`:
      `agent_tool.rs`, `ask_user_question.rs`, `bash.rs`,
      `edit.rs`, `enter_plan_mode.rs`, `enter_worktree.rs`,
      `exit_plan_mode.rs`, `exit_worktree.rs`, `glob_tool.rs`,
      `grep.rs`, `read.rs`, `send_message.rs`, `sleep_tool.rs`,
      `task_create.rs`, `task_get.rs`, `task_list.rs`,
      `task_output.rs`, `task_stop.rs`, `task_update.rs`,
      `team_create.rs`, `team_delete.rs`, `todo_write.rs`,
      `tool_search.rs`, `web_fetch/`, `web_search.rs`, `write.rs`.
      Replace the `cancel: &CancellationToken` parameter with
      `ctx: &ToolContext`; where the tool body references `cancel`,
      replace with `&ctx.cancel`. No other behaviour change.
- [ ] 2.4 Update every `Tool::execute` call site in the workspace
      (cc-query `engine.rs` tool-dispatch; cc-agents tool driver
      if any; tool integration tests). Run `rg -n "\.execute\(" |
      grep -v -F '.rs:'$' '` to find them all.
- [ ] 2.5 In `engine.rs` tool-call dispatch, construct a
      `ToolContext` per call:
      ```
      let ctx = ToolContext {
          session: self.session_handle.clone(),
          cancel: cancel.clone(),
          message_id: Some(assistant_msg_id.clone()),
      };
      tool.execute(tu.input.clone(), &ctx).await
      ```
      This requires `Session` to be behind `Arc` inside `QueryEngine`
      — today it's owned (`session: Session`). Wrap in `Arc<Session>`
      at construction time (struct field change + `QueryEngineConfig`
      accepts `Arc<Session>`; `Session::append*` takes `&self` so
      this is safe).
- [ ] 2.6 Alternative: if Option A feels too broad, limit the
      change to Option B (add a side-channel
      `SessionAwareTool` trait impl only for Edit/Write). Document
      the chosen option in the commit message.

## 3. File-history snapshot — Write tool

- [ ] 3.1 In `rust/crates/cc-tools/src/write.rs` `execute`, before
      the file write (after path resolution, before the atomic
      write), if `path.exists()` and `!ctx.message_id.is_none()`:
      - `let prior = std::fs::read(&path)?` (size-bounded; use a
        512 MiB hard cap to avoid pathological writes)
      - Build a `FileHistoryBackup { backup_file_name: relpath,
        backup_time: SystemTime::now(), is_snapshot_update: false,
        content: prior }`.
      - Build a `FileHistorySnapshot` with `message_id = ctx.
        message_id.clone().unwrap_or_default()` and
        `tracked_file_backups = BTreeMap::from([(relpath,
        backup)])`.
      - Call `ctx.session.append_file_history_snapshot(&snap,
        false)?`.
      - Continue to the existing write.
- [ ] 3.2 Net-new file (path did not exist before): do NOT write a
      snapshot. Matches TS.

## 4. File-history snapshot — Edit tool

- [ ] 4.1 In `rust/crates/cc-tools/src/edit.rs` `execute`, at the
      first stat (existing-file precondition), after verifying the
      file exists and before applying the replacement, build and
      append the same `FileHistorySnapshot` shape as in §3.1. Use
      the bytes captured for the existing (len, mtime) pair so the
      snapshot content is byte-identical to what the model sees.
- [ ] 4.2 Edit failures (old_string not found, non-unique match,
      lost-update detection, cancel mid-edit): do NOT append a
      snapshot — snapshot is only for **successful** edits to match
      TS's "post-success side-effect" semantics.

## 5. (Optional, second commit) is_snapshot_update rule

- [ ] 5.1 Add `Session::has_snapshot_for_path_in_turn(&relpath) ->
      bool` + `Session::start_turn() / end_turn()` to track a
      per-turn `HashSet<String>` of relpaths already snapshotted.
      cc-query calls `start_turn` on user-message push and
      `end_turn` on turn completion (both success and
      `CcError::Cancelled`). Edit / Write pass the snapshot update
      flag as `true` when
      `has_snapshot_for_path_in_turn(&relpath)` is already `true`,
      and insert the path into the set after writing.
- [ ] 5.2 If skipped, `is_snapshot_update` stays hard-coded `false`
      everywhere, matching current behaviour. Not blocking for
      correctness — affects replay granularity only.

## 6. Tests

- [ ] 6.1 `cc-query` `engine::tests::cancel_during_tool_use_appends
      _canonical_markers` — set up engine, fire cancel mid-stream
      after a `tool_use` header, assert:
      (a) session JSONL's assistant content block contains
      `INTERRUPT_MESSAGE` verbatim;
      (b) synthetic tool_result's content string equals
      `INTERRUPT_MESSAGE_FOR_TOOL_USE`;
      (c) `Session::load_transcript_entries()` includes a
      `SessionEntry::Message` variant whose content matches the
      marker string (produced by `append_interrupt_marker`).
- [ ] 6.2 `cc-query` `engine::tests::cancel_without_tool_use
      _appends_plain_marker` — cancel fires with text only, no
      tool_use. Assert (a) is present; assert (b) tool_result is
      absent; assert (c) standalone marker is plain (not tool-use
      variant).
- [ ] 6.3 `cc-tools` `edit::tests::successful_edit_appends_file
      _history_snapshot` — write a file on disk, execute Edit to
      replace a substring, call session reader, assert exactly one
      `FileHistorySnapshot` entry exists with
      `tracked_file_backups[relpath].content == original_bytes`
      and `is_snapshot_update == false`.
- [ ] 6.4 `cc-tools` `edit::tests::failed_edit_does_not_snapshot`
      — Edit fails (old_string not found); assert zero
      `FileHistorySnapshot` entries were written.
- [ ] 6.5 `cc-tools` `write::tests::write_overwrite_appends
      _snapshot` — pre-existing file, Write succeeds, assert one
      snapshot with original bytes.
- [ ] 6.6 `cc-tools` `write::tests::write_new_file_does_not
      _snapshot` — target path did not exist; Write succeeds;
      assert zero snapshots (matches TS).
- [ ] 6.7 `cc-core` `tool::tests::tool_context_struct_init` —
      construct `ToolContext` with session + cancel + message_id;
      passed to a dummy tool `execute`; tool reads all three
      correctly.

## 7. Verification

- [ ] 7.1 `cargo fmt --all` clean.
- [ ] 7.2 `cargo clippy --workspace --all-targets -- -D warnings`
      clean.
- [ ] 7.3 `cargo test --workspace` — all green, including the
      pre-existing 24 cc-session tests.
- [ ] 7.4 Manual resume smoke: start a session, run a Write tool,
      Ctrl+C during streaming, resume the session via `claude
      --resume <id>`. Confirm: (a) resume loads; (b) session list
      shows the interrupt marker is visible; (c) session JSONL
      shows the file-history-snapshot entry with pre-edit bytes.

## 8. Sign-off

- [ ] 8.1 Commit message references P0 #5 (interrupt marker) and
      P0 #6 (file-history snapshot) end-to-end closure.
- [ ] 8.2 Update `.claude/plan/parity-gaps-2026-04-23.md`:
      - P0 #5 and #6 rows: cross-reference both
        `fix-session-resume-integrity` (types) and this change
        (wiring).
      - Note that `fix-session-resume-integrity` is now fully
        live once this merges.
- [ ] 8.3 Update memory `project_phase3_progress.md` Batch B
      paragraph to flip P0 #5/#6 from "dead plumbing" to
      "end-to-end live".
- [ ] 8.4 Follow-up issues (optional):
      - WebEdit / SedEdit parity (TS has these; Rust doesn't yet).
      - `is_snapshot_update` per-turn tracking (covered in §5).
