## ADDED Requirements

### Requirement: JSONL Entry Union Round-Trips All Written Variants

`cc_session` SHALL deserialise every JSONL entry variant the TS writer
emits in normal operation. Specifically, a `SessionEntry` union parses:

- message turns (`TranscriptEntry` — user/assistant with optional
  usage/compact-boundary);
- `summary`, `custom-title`, `ai-title`, `last-prompt`, `task-summary`,
  `tag`, `agent-name`, `agent-color`, `agent-setting`, `pr-link`,
  `file-history-snapshot`, `attribution-snapshot`, `mode`,
  `worktree-state` meta entries;
- any other `type`-tagged shape → `SessionEntry::Unknown(Value)` carrying
  the raw JSON so the entry is preserved across resume instead of being
  silently dropped.

`Session::load_messages` SHALL continue to return only the message
entries (signature unchanged). A new `Session::load_transcript_entries`
SHALL return the full `Vec<SessionEntry>` so meta lookups (tags, PR
links, file-history snapshots) do not require reopening the file.

#### Scenario: Known meta entries round-trip
- **GIVEN** a JSONL line `{"type":"tag","sessionId":"<uuid>","tag":"demo"}`
- **WHEN** `load_transcript_entries` reads it
- **THEN** the result contains a `SessionEntry::Meta(MetaEntry::Tag { … })`
- **AND** re-serialising the entry produces a JSON value equal to the
  original (modulo key order).

#### Scenario: Unknown variants are preserved, not dropped
- **GIVEN** a JSONL line with `type: "marble-origami-commit"` (experimental)
- **WHEN** `load_transcript_entries` reads it
- **THEN** the result contains a `SessionEntry::Unknown(raw)` holding the
  full JSON object — no entry is silently discarded.

#### Scenario: Malformed line tolerance stays
- **GIVEN** a JSONL transcript with one valid entry followed by a
  truncated / non-JSON trailing line
- **WHEN** `load_transcript_entries` reads it
- **THEN** the valid entry is returned and the bad line is logged at
  WARN but does not fail the load.

### Requirement: Canonical Interrupt-Marker Append

`cc_session` SHALL export `INTERRUPT_MESSAGE` = `"[Request interrupted
by user]"` and `INTERRUPT_MESSAGE_FOR_TOOL_USE` = `"[Request interrupted
by user for tool use]"` as public `const &str` matching the TS wire
literals exactly.

`Session::append_interrupt_marker(for_tool_use: bool)` SHALL append a
user-role message whose single text block contains the matching
constant, persisted through the same durable append path as ordinary
turns.

#### Scenario: Interrupt marker is reloadable
- **GIVEN** a live session that has written one user and one assistant turn
- **WHEN** `append_interrupt_marker(false)` is called and the session is
  reloaded
- **THEN** `load_messages` returns three messages, the last one a user
  message with text exactly `"[Request interrupted by user]"`.

### Requirement: FileHistorySnapshot Persistence

`cc_session` SHALL persist file-history snapshots as dedicated
`file-history-snapshot` JSONL entries matching the TS shape (`messageId`,
`snapshot.trackedFileBackups`, `snapshot.timestamp`, `isSnapshotUpdate`).

A write helper `Session::append_file_history_snapshot(&snapshot,
is_update)` and a read helper `Session::file_history_snapshots() ->
Vec<FileHistorySnapshotMessage>` SHALL be exposed so callers can recover
edited-file state on resume.

#### Scenario: Append then recover
- **GIVEN** a session that has appended two snapshots via
  `append_file_history_snapshot`
- **WHEN** a fresh `Session::resume` is performed and
  `file_history_snapshots` is called
- **THEN** the returned vector has two entries in write order with the
  original `messageId`s and backup maps intact.

### Requirement: Headless Resume Merges Resumed Messages

When `claude --resume <id>` (or `--continue`) is invoked in headless
mode without `--message`:

- if stdin has data, that data SHALL become the user text for the next
  turn (same behaviour as new sessions);
- if stdin is a tty (no piped data), the CLI MAY error with a clear
  "pipe a message or pass --message" hint, but MUST NOT reject the
  combination of `--resume` + piped stdin up-front;
- the resumed `Vec<MessageParam>` returned by `Session::resume` SHALL be
  passed to `QueryEngine::run_turn` as the `initial_messages` vector so
  the API call carries full conversation history.

#### Scenario: Resume + piped stdin works
- **GIVEN** a saved session with at least one user/assistant exchange
- **WHEN** `echo "next turn" | claude --resume <id>` runs
- **THEN** the CLI does NOT error on resolution
- **AND** the query engine is called with `initial_messages.len() >= 2`
  (the prior turns) plus the new user turn.

#### Scenario: Resume + tty + no --message fails with a useful error
- **GIVEN** stdin is a tty and no `--message` is passed
- **WHEN** `claude --resume <id>` runs in headless mode
- **THEN** the CLI exits with an error message that mentions either
  `--message` or "pipe" — it does NOT attempt to proceed with empty text.
