## Why

Four parity gaps in `cc-session` + the `cc` binary combine to make
`--resume` a lossy / partially-broken path today. Roadmap rows P0 #4,
#5, #6, #19 in `.claude/plan/parity-gaps-2026-04-23.md`.

### 1. JSONL entry union truncated (P0 #4)

`cc-session/src/lib.rs:44-54` only deserialises `TranscriptEntry` — a
turn-shaped record with `{message, timestamp, usage?, compact_boundary?}`.
The TS schema at `src/types/logs.ts:297-317` is a much larger union:
`summary`, `file-history-snapshot`, `attribution-snapshot`, `pr-link`,
`tag`, `agent-name`, `agent-color`, `agent-setting`, `mode`,
`worktree-state`, `last-prompt`, `task-summary`, `ai-title`,
`custom-title`, plus experimental `marble-origami-*`,
`content-replacement`, `speculation-accept`, `queue-operation`.

`Session::load_messages` is tolerant enough to skip unparseable lines
with a warning, so today these simply vanish on resume: tags, PR links,
file-history snapshots, mode flags — all dropped silently. The caller
cannot recover them even if it wanted to.

### 2. No interrupt-marker append path (P0 #5)

TS exports `INTERRUPT_MESSAGE = '[Request interrupted by user]'` and
`INTERRUPT_MESSAGE_FOR_TOOL_USE` from `src/utils/messages.ts:207-209`
and appends a user turn carrying that literal text whenever the user
cancels mid-query. Resume uses the exact string to detect "the previous
turn was aborted — don't re-dispatch a pending tool_use".

Rust `cc-query/src/engine.rs:220-272` writes a bespoke
`"[Interrupted by user]"` (different wording!) inline but:
  - the string lives nowhere that other call sites can import, so TUI
    Ctrl+C / headless SIGINT paths each invent their own variant;
  - `cc-session` offers no dedicated append API, so callers have to
    open-code a `MessageParam::user(...)` + `session.append(...)`
    pair, which means the marker can drift out of sync across sites.

### 3. FileHistorySnapshot not persisted (P0 #6)

TS `src/utils/fileHistory.ts:39-52` defines a per-edit snapshot record
that `sessionStorage.insertFileHistorySnapshot` writes to the transcript
as a `file-history-snapshot` JSONL entry. On resume, TS recovers the
edited-file backup map so `/undo`, `/diff`, and the Edit tool's
`originalFile` reference work across sessions.

Rust has no `FileHistorySnapshot` type, no append path, and no reader.
Edited-file state is entirely lost on resume — a silent data-loss bug.

### 4. `--resume` doesn't merge resumed messages into headless
  `initial_messages` (P0 #19)

`rust/cc/src/main.rs:225-233` returns an error in the headless path
when `--resume` is set and `--message` is absent — even when stdin is
a pipe that could supply the next turn:

```rust
if atty_is_stdin() && resume_id.is_none() {
    return Err("no message provided — use --message or pipe text via stdin".into());
}
if resume_id.is_some() && cli.message.is_none() {
    return Err("--resume/--continue without TUI requires --message …".into());
}
```

The second guard is too broad: `echo hi | claude --resume X` currently
errors even though stdin has text, and `claude --resume X` with no
stdin errors even though an interactive user in TUI mode continues
just fine. TS behaviour: if stdin has content, use it as the next
turn; otherwise require `--message`. The resumed messages then flow
into the query engine as `initial_messages`.

## What Changes

### 1. Port the TS entry union (P0 #4)

Introduce `SessionEntry` — an untagged enum over:
  - `Message(TranscriptEntry)` — the existing turn record (back-compat).
  - `Meta(MetaEntry)` — an internally-tagged enum keyed on the `type`
    field covering: `summary`, `custom-title`, `ai-title`, `last-prompt`,
    `task-summary`, `tag`, `agent-name`, `agent-color`, `agent-setting`,
    `pr-link`, `file-history-snapshot`, `attribution-snapshot`, `mode`,
    `worktree-state`. Experimental variants not in this list deserialise
    into `Unknown(serde_json::Value)` with the raw JSON preserved.
  - `Unknown(serde_json::Value)` — catch-all so forward-compat writes
    (e.g. `marble-origami-*`) round-trip through resume without being
    silently dropped. Target **80% parity**, not 100%: the variants above
    are the ones TS writes in every run; the rest are experimental gates.

`Session::load_transcript_entries(&self) -> CcResult<Vec<SessionEntry>>`
returns all entries. `Session::load_messages` stays as-is (only returns
message entries) so existing callers are unchanged.

### 2. Canonicalise the interrupt marker (P0 #5)

Export `cc_session::INTERRUPT_MESSAGE` and
`cc_session::INTERRUPT_MESSAGE_FOR_TOOL_USE` as `const &str`. Add
`Session::append_interrupt_marker(for_tool_use: bool) -> CcResult<()>`
that appends a user message carrying the canonical literal. Callers
(cc-query, cc-tui, future SIGINT handler) route through this one
function.

### 3. Persist + read file-history snapshots (P0 #6)

Add `FileHistoryBackup` + `FileHistorySnapshot` structs matching the TS
shapes (message_id, tracked_file_backups, timestamp). The
`file-history-snapshot` entry falls under the `MetaEntry` union above,
so persistence is `Session::append_file_history_snapshot(&snap,
is_update: bool)` → one JSONL line. Read helper
`Session::file_history_snapshots(&self) -> Vec<FileHistorySnapshotMessage>`
filters the entry stream. We do **not** add the snapshots to
`SessionMetadata` — they are per-edit events, not per-session state, so
the JSONL-entry shape mirrors TS exactly.

### 4. Resume merges into headless initial_messages (P0 #19)

`rust/cc/src/main.rs`: replace the two error returns at lines 225-233
with a decision that mirrors TS:

  - `--resume` + `--message` → use `--message` verbatim.
  - `--resume` + stdin has data → use stdin.
  - `--resume` + interactive (tty, no `--message`) → outside headless
    mode (TUI handles resume natively); headless still errors with a
    clearer message.
  - new session (no `--resume`) + tty + no message → error (unchanged).

Resumed messages continue to flow into `engine.run_turn(... &mut
messages ...)` as they already do at line 491/507 — that path is
correct; the bug is that the guard above returns early before it.

## Capabilities

### Modified Capabilities
- `session-persistence`: resume SHALL round-trip the full set of JSONL
  entry types TS writes, preserve unknown entries as opaque JSON, expose
  a canonical interrupt-marker append path, and persist per-edit
  file-history snapshots.

## Impact

- **Affected code:** `rust/crates/cc-session/src/lib.rs` (entry union,
  interrupt const + append method, file-history snapshot types + append
  + read), `rust/cc/src/main.rs` (headless resume merge decision).
- **cc-query / cc-tui:** unchanged in this batch — cc-query's existing
  `"[Interrupted by user]"` literal in `engine.rs` remains wrong-wording
  and separate; migrating it onto the new constant is a follow-up so
  this change stays scoped to `cc-session` + `cc` per the roadmap's
  "no speculative abstractions" rule.
- **Risk:** LOW-MEDIUM. The new entry enum is additive;
  `Session::load_messages` keeps its exact shape. The resume-merge
  change is a handful of lines on a path users already rely on, and is
  covered by a new integration test.
