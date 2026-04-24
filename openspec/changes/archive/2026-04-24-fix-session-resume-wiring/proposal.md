# Proposal — Wire the interrupt-marker producer into cc-query cancel path

> **Scope (narrowed 2026-04-23).** This change is now only the
> interrupt-marker slice of the original proposal. The file-history-
> snapshot wiring, the `Tool::execute` signature migration, and the
> cancel-path regression tests have been split out into three
> follow-up changes:
>
> - `fix-tool-context-refactor` — `ToolContext` struct + `Tool::
>   execute` migration + `Session` behind `Arc`.
> - `fix-engine-stream-mock-harness` — test-only stream-producer
>   override on `QueryEngine` so the cancel-path tests (and the
>   deferred `fix-hook-correctness-wiring` §5.3) can drive full
>   turns without hitting a live API.
> - `fix-file-history-snapshot-producers` — Edit / Write call
>   `append_file_history_snapshot` on successful mutation, plus the
>   two cancel-path regression tests.
>
> Archive this change once `fix-file-history-snapshot-producers`
> merges — at that point the cancel-path markers are fully tested
> by the follow-up and this change has no open obligations.

## Why

`fix-session-resume-integrity` (commits `20c0c3f` + `b0208be`,
2026-04-23) shipped `INTERRUPT_MESSAGE` /
`INTERRUPT_MESSAGE_FOR_TOOL_USE` pub consts and a
`Session::append_interrupt_marker(for_tool_use: bool)` helper that
writes the marker as a user message through the durable append
path.

The QA pass on 2026-04-23 found the helper had zero production
callers. `rust/crates/cc-query/src/engine.rs:222-269` still wrote
the partial-interrupt marker as a **hard-coded
`"\n[Interrupted by user]"`** literal baked into the assistant's
content block (line 224) + into synthetic `tool_result` error
strings (line 246). That wording differs from the TS canonical
`"[Request interrupted by user]"` / `"[Request interrupted by user
for tool use]"` and bypassed the new `Session` helper entirely —
which meant parity gap **P0 #5** was still open even though the
machinery was in place.

## Goal

Migrate the two hard-coded strings to the new consts, then call
`Session::append_interrupt_marker(...)` after the partial-content
appends so resume-detection keys on the canonical entry.

Not in scope (moved to follow-ups; see banner above):

- Edit / Write file-history snapshot emission.
- Changes to the `Tool::execute` signature or `QueryEngine`
  session ownership.
- Cancel-path regression tests — they need the mock-stream harness
  that `fix-engine-stream-mock-harness` adds.

## What changes

### cc-query cancel path — `engine.rs:222-270`

Before (pre-change):

```rust
if cancel.is_cancelled() && !text_buf.is_empty() {
    let mut interrupted_content = message.content.clone();
    interrupted_content.push(ContentBlock::text("\n[Interrupted by user]"));
    let interrupted_tool_results: Vec<ContentBlock> = interrupted_content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::ToolUse(tu) => Some(ContentBlock::ToolResult(ToolResultBlock {
                tool_use_id: tu.id.clone(),
                content: Some(Value::String("[Interrupted by user]".to_string())),
                is_error: Some(true),
                cache_control: None,
            })),
            _ => None,
        })
        .collect();
    // self.session.append(&partial_msg); self.session.append(&result_msg);
    return Err(CcError::Cancelled);
}
```

After:

1. Import the consts: `use cc_session::{INTERRUPT_MESSAGE,
   INTERRUPT_MESSAGE_FOR_TOOL_USE};`
2. Replace line 224 with `format!("\n{}", INTERRUPT_MESSAGE)`.
3. Replace the literal at line 246 with
   `INTERRUPT_MESSAGE_FOR_TOOL_USE.to_string()`.
4. After both `self.session.append(...)` calls, append the
   canonical marker:
   ```rust
   self.session.append_interrupt_marker(
       !interrupted_tool_results.is_empty(),
   )?;
   ```
   This must run after the content blocks are persisted so a
   concurrent reader never sees marker-without-content.

### Non-changes

- `cc-tui/src/render.rs:609` ("esc to interrupt · ctrl+c to
  cancel") stays as-is. That's streaming-mode help footer text,
  not a resume marker.
- `Session::append` paths and JSONL durability rules are
  untouched.

## Impact

- **Affected specs**: `session-resume-wiring` capability (this
  change) gets a single Requirement for the cancel-path producer.
- **Affected crates**: `cc-query` only.
- **Compatibility**: additive. Existing JSONL readers already know
  how to deserialise the marker entry (it landed with
  `fix-session-resume-integrity`).
- **Testing**: regression tests for this producer live in
  `fix-file-history-snapshot-producers` (they need the stream-
  mock harness). Until those land, the producer's correctness is
  verified by code inspection + the two manual scenarios in the
  spec.
