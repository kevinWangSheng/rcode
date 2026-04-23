## 1. JSONL entry union (P0 #4)

- [x] 1.1 Introduce `SessionEntry` enum in `cc-session/src/lib.rs`:
      `Message(TranscriptEntry) | Meta(MetaEntry) | Unknown(Value)`,
      `#[serde(untagged)]` at the top level.
- [x] 1.2 Introduce `MetaEntry` as an internally-tagged enum
      (`#[serde(tag = "type", rename_all = "kebab-case")]`) covering the
      14 TS variants TS writes in normal operation. Defer the four
      experimental types (marble-origami-*, content-replacement,
      speculation-accept, queue-operation) — they land in `Unknown`.
- [x] 1.3 Add `Session::load_transcript_entries(&self) ->
      CcResult<Vec<SessionEntry>>` that reuses the existing
      line-tolerant loader.
- [x] 1.4 Keep `Session::load_messages` signature untouched — it
      filters to `SessionEntry::Message`.
- [x] 1.5 Serde round-trip test per supported variant + one
      `Unknown` case + one `TranscriptEntry` back-compat case.

## 2. Interrupt marker (P0 #5)

- [x] 2.1 Export `INTERRUPT_MESSAGE` and `INTERRUPT_MESSAGE_FOR_TOOL_USE`
      as public `const &str` matching the TS literals exactly.
- [x] 2.2 Add `Session::append_interrupt_marker(for_tool_use: bool) ->
      CcResult<()>` that appends a user `MessageParam` with content
      matching the constant. Uses the existing durable append path.
- [x] 2.3 Test: append marker, reload via `load_messages`, assert the
      last entry is a user message whose text matches the constant.

## 3. File-history snapshot (P0 #6)

- [x] 3.1 Add `FileHistoryBackup` + `FileHistorySnapshot` +
      `FileHistorySnapshotMessage` types in `cc-session/src/lib.rs` with
      fields matching TS `fileHistory.ts:33-52` (camelCase wire names
      preserved via `#[serde(rename_all = "camelCase")]`).
- [x] 3.2 Add `Session::append_file_history_snapshot(&self, snapshot:
      &FileHistorySnapshot, is_update: bool)` → emits a JSONL line with
      `type: "file-history-snapshot"` via the MetaEntry variant.
- [x] 3.3 Add `Session::file_history_snapshots(&self) ->
      CcResult<Vec<FileHistorySnapshotMessage>>` that scans the
      transcript and returns the ordered list.
- [x] 3.4 Test: append two snapshots, read back, assert order and
      contents.

## 4. Resume merges into headless initial_messages (P0 #19)

- [x] 4.1 In `rust/cc/src/main.rs::headless_user_text` resolution, drop
      the blanket `resume_id.is_some() && cli.message.is_none()` error.
      Only error when we truly cannot supply a user turn (tty + no
      --message on a brand-new session).
- [x] 4.2 When `--resume` is set and `--message` is absent, allow
      stdin fallback — same as new sessions get today.
- [x] 4.3 Confirm existing `engine.run_turn(... &mut messages ...)`
      call already threads resumed messages through — no code change in
      `cc-query`.
- [x] 4.4 Factor the message-resolution logic into a pure helper
      (`resolve_headless_user_text(resume_id, cli_message, stdin_reader,
      is_tty)`) so it is unit-testable without spawning a real CLI.
- [x] 4.5 Unit tests covering the four decision branches
      (resume+message, resume+stdin, new+message, new+tty error).

## 5. Verify

- [x] 5.1 `cd rust && cargo check -p cc-session -p cc` clean.
- [x] 5.2 `cd rust && cargo clippy -p cc-session -p cc -- -D warnings`
      clean.
- [x] 5.3 `cd rust && cargo test -p cc-session` clean.

## 6. Sign-off

- [x] 6.1 Commit inside the worktree. Do NOT push — parent session
      merges.
