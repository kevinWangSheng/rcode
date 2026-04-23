# Spec — Session-resume producers

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

### Requirement: Edit and Write tools persist a FileHistorySnapshot on successful mutation of an existing file

The Edit and Write tools MUST persist a `FileHistorySnapshot` entry
in the session when they successfully mutate a pre-existing file.
Specifically, when the Edit or Write tool successfully mutates a file
that existed on disk at the start of the turn, it SHALL append a
`FileHistorySnapshot` entry to the session via
`Session::append_file_history_snapshot(&snap, false)` containing:

- `message_id = ctx.message_id.clone().unwrap_or_default()`
- `tracked_file_backups = { relpath: FileHistoryBackup { … } }` with
  exactly one entry for the file being mutated
- `FileHistoryBackup.backup_file_name = relpath` (relative to the
  project root)
- `FileHistoryBackup.backup_time = SystemTime::now()`
- `FileHistoryBackup.is_snapshot_update = false`
- `FileHistoryBackup.content` equals the pre-mutation bytes of the
  file

The tool SHALL NOT append a snapshot when:

- the file did not exist prior to the mutation (net-new Write / Edit
  create)
- the mutation failed for any reason (validation error, concurrent-
  write detection, cancel, permission denial)
- the tool context has no `session` reference (isolated tool tests
  use a best-effort skip)

The snapshot append SHALL happen on the successful-write side-
effect path AFTER the filesystem write but BEFORE returning the
`ToolResult`, so a resume that sees the snapshot also sees a
filesystem that actually changed.

#### Scenario: Successful Write over existing file

- **Given** `/foo.txt` exists with contents `"old"`
- **When** `WriteTool::execute` runs with `{"file_path":"/foo.txt",
  "content":"new"}`
- **Then** `/foo.txt` on disk now contains `"new"`
- **And** the session JSONL contains one `FileHistorySnapshot`
  entry whose `tracked_file_backups["foo.txt"].content == b"old"`
  and `is_snapshot_update == false`

#### Scenario: Successful Edit over existing file

- **Given** `/bar.rs` exists with contents `"fn a() {}\nfn b() {}\n"`
- **When** `EditTool::execute` runs with `{"file_path":"/bar.rs",
  "old_string":"fn a", "new_string":"fn x"}`
- **Then** `/bar.rs` on disk is `"fn x() {}\nfn b() {}\n"`
- **And** one `FileHistorySnapshot` entry is in the session JSONL
  with the pre-edit bytes

#### Scenario: Write of a net-new file does not snapshot

- **Given** `/new.txt` does not exist
- **When** `WriteTool::execute` creates it with `{"content":"hi"}`
- **Then** the write succeeds
- **And** zero `FileHistorySnapshot` entries are appended (matches
  TS semantics for first-write)

#### Scenario: Failed Edit does not snapshot

- **Given** `/qux.txt` exists with contents `"abc"`
- **When** `EditTool::execute` runs with an `old_string` that does
  not appear in the file and returns an error
- **Then** the file on disk is unchanged
- **And** zero `FileHistorySnapshot` entries are appended

### Requirement: Tool trait exposes a session-bearing context

`cc_core::Tool::execute` SHALL accept a `ToolContext` reference
instead of a bare `CancellationToken`. `ToolContext` SHALL carry
at minimum `session: Arc<cc_session::Session>`,
`cancel: CancellationToken`, and `message_id: Option<String>`. The
cc-query dispatch site SHALL construct a fresh `ToolContext` per
tool call, populating `message_id` with the current assistant
turn's id.

The engine SHALL retain `Session` behind `Arc` so the handle can be
cloned into each `ToolContext` without exclusive ownership
conflicts.

#### Scenario: Tool observes the context

- **Given** a dummy `Tool` implementation that records the ctx it
  receives
- **When** the cc-query dispatch site calls `tool.execute(input,
  &ctx)` during a turn
- **Then** the dummy's recorded ctx has the expected
  `message_id.is_some()` and the session handle points to the same
  instance the engine holds
