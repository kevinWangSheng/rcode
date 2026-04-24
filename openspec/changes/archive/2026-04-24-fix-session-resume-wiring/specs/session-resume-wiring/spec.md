# Spec — Session-resume producers (interrupt-marker slice)

> **Scope (narrowed 2026-04-23):** this spec now covers only the
> interrupt-marker producer. The two Requirements previously here —
> "Edit and Write tools persist a FileHistorySnapshot …" and "Tool
> trait exposes a session-bearing context" — moved to follow-up
> changes:
>
> - `fix-tool-context-refactor` owns the Tool-trait /
>   `ToolContext` Requirement.
> - `fix-file-history-snapshot-producers` owns the Edit/Write
>   Requirement + the cancel-path regression-test Requirement.
>
> This change may be archived after
> `fix-file-history-snapshot-producers` lands.

## ADDED Requirements

### Requirement: cc-query cancel path emits canonical interrupt markers

The cc-query engine MUST emit canonical interrupt markers when a
streaming turn is cancelled after some content has arrived.
Specifically, when the query engine's streaming loop is interrupted
by cancellation AND some content has already been received, the
engine SHALL:

1. Use `cc_session::INTERRUPT_MESSAGE` (not a literal string) as the
   suffix in the partial assistant content block saved to the session.
2. Use `cc_session::INTERRUPT_MESSAGE_FOR_TOOL_USE` (not a literal
   string) as the content of any synthetic `tool_result` stubs
   emitted for dangling `tool_use` blocks.
3. After appending the partial assistant message (and optionally the
   synthetic tool-result user message), call
   `Session::append_interrupt_marker(for_tool_use)` where
   `for_tool_use == true` iff any synthetic tool-result stub was
   written.

The engine SHALL NOT call `append_interrupt_marker` before the
partial-content appends complete; ordering is load-bearing for
resume detection.

#### Scenario: Cancel with dangling tool_use

- **Given** a streaming turn that has emitted at least one
  `tool_use` block before cancellation fires
- **When** `run_turn` handles the cancellation at `engine.rs:222`
- **Then** the session JSONL now contains, in order:
  1. A `Message` entry for the partial assistant turn whose last
     content block is a text block containing
     `cc_session::INTERRUPT_MESSAGE`
  2. A `Message` entry for the synthetic `tool_result` stubs where
     each stub's content equals `cc_session::INTERRUPT_MESSAGE_FOR
     _TOOL_USE`
  3. A canonical interrupt-marker entry equivalent to
     `append_interrupt_marker(true)` (tool-use variant)

#### Scenario: Cancel without tool_use

- **Given** a streaming turn that has emitted text only, no
  `tool_use` block, before cancellation
- **When** `run_turn` handles the cancellation
- **Then** the session JSONL contains:
  1. A `Message` entry for the partial assistant turn ending with
     the `INTERRUPT_MESSAGE` text block
  2. A canonical interrupt-marker entry equivalent to
     `append_interrupt_marker(false)` (plain variant)
- **And** no synthetic `tool_result` stub is present
