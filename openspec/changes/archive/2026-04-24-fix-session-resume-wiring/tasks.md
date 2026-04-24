> **Scope (narrowed 2026-04-23):** Only §1 remains in this change.
> The original §2–§8 (ToolContext refactor, Edit/Write snapshot
> producers, is_snapshot_update rule, cancel-path regression tests)
> have been split into three follow-up changes to keep review units
> focused:
>
> - `fix-tool-context-refactor` — §2 (Tool::execute signature
>   migration + Session behind Arc).
> - `fix-engine-stream-mock-harness` — test infrastructure that
>   unblocks the cancel-path regression tests (and the deferred
>   §5.3 in `fix-hook-correctness-wiring`).
> - `fix-file-history-snapshot-producers` — §3 / §4 / §6 (Edit +
>   Write emit `FileHistorySnapshot`; cancel-path regression
>   tests). Depends on the two above.
>
> §5 (`is_snapshot_update` per-turn tracking) is intentionally
> deferred as polish; open an issue if replay granularity becomes
> a user-visible concern.
>
> This change may be archived after
> `fix-file-history-snapshot-producers` lands — at that point its
> coverage (cancel-path markers) is owned by that change's §5
> regression tests.

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
