## 1. Append Path

- [x] 1.1 After `writeln!(file, "{line}")?` in
      `cc-session/src/lib.rs::append`, call `file.sync_all()` and
      propagate the error via `CcError::io`.
- [x] 1.2 Audit sibling writers in the same module
      (`write_metadata`, any todo-list persistence path) and apply the
      same pattern. (N/A: no sibling writers exist in current
      `cc-session`; only the transcript JSONL is persisted.)

## 2. Metadata Writes

- [x] 2.1 For `metadata.json` (written with `fs::write`), switch to the
      atomic tmpfile + rename pattern so a crash mid-write does not leave
      a truncated `metadata.json`. (N/A: no metadata.json path exists in
      current `cc-session` — only the append-only transcript.)
- [x] 2.2 Keep `fs::write` semantics for read-only tests where durability
      doesn't matter.

## 3. Durability Test

- [x] 3.1 New `cc-session/tests/durability.rs` that spawns a child
      process, has it append two messages, then `SIGKILL`s it, then
      reopens the transcript in the parent and asserts both messages
      survived.
- [x] 3.2 Cross-check: if the test cannot reliably trigger loss without
      the fix (e.g., tokio's page cache hides it on the current FS), add
      a loom or `unsafe { libc::exit(9) }` variant to force it.
      Implemented the `libc::_exit(9)` variant.

## 4. Docs

- [ ] 4.1 Update `.claude/plan/implementation-notes.md` with a line about
      the fsync contract so future refactors don't drop it. (Skipped:
      edit permission denied in this task's scope; contract is documented
      in the `append` method doc-comment instead.)

## 5. Sign-off

- [x] 5.1 `cargo test -p cc-session` + `cargo test --workspace` +
      `cargo clippy --workspace -- -D warnings` clean.
