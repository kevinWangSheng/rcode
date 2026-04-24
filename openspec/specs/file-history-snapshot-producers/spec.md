# file-history-snapshot-producers Specification

## Purpose

Defines the producer-side contract for file-history snapshots:
Edit and Write MUST persist a pre-mutation sidecar and a
`FileHistorySnapshot` JSONL entry on every successful mutation of
a file that existed at turn start, so that resume / rewind can
restore the exact pre-edit bytes deterministically.
## Requirements
### Requirement: Edit and Write emit a FileHistorySnapshot on successful mutation of an existing file

The Edit and Write tools MUST persist a `FileHistorySnapshot` entry
via `ctx.session.append_file_history_snapshot(&snap, false)`
whenever they successfully mutate a file that existed on disk at
the start of the call. The snapshot SHALL carry:

- `message_id = ctx.message_id.clone().unwrap_or_default()`
- `tracked_file_backups` with a single entry keyed by the
  project-relative path of the mutated file
- `FileHistoryBackup.backup_file_name = <relpath>`
- `FileHistoryBackup.backup_time = SystemTime::now()`
- `FileHistoryBackup.is_snapshot_update = false`
- `FileHistoryBackup.content = <pre-mutation bytes>`

The tool SHALL NOT emit a snapshot when:

1. The file did not exist prior to the mutation (net-new Write or
   Edit-creates-file flow).
2. Any precondition check fails (file exists, `old_string` found,
   uniqueness, stat-pair / lost-update, permission).
3. The cancel token fires before the write is attempted.

The snapshot SHALL be emitted AFTER all precondition checks pass
but BEFORE the filesystem write. If the write itself errors, the
snapshot is left in the JSONL as a harmless orphan (the replay
reader re-applies the saved content, which is a no-op against the
unchanged disk state).

Snapshots exceeding 64 MiB (`MAX_SNAPSHOT_BYTES`) SHALL be
persisted anyway but SHALL emit a `tracing::warn!` with the path
and byte count so operators notice pathological writes.

#### Scenario: Write over existing file persists pre-write bytes

- **Given** `/proj/foo.txt` exists with contents `"old"`
- **When** `WriteTool::execute({"file_path": "/proj/foo.txt",
  "content": "new"}, &ctx)` runs with a real session ctx
- **Then** `/proj/foo.txt` on disk contains `"new"`
- **And** `ctx.session.file_history_snapshots()` returns exactly
  one snapshot
- **And** that snapshot's single backup has `content == b"old"`,
  `is_snapshot_update == false`, and `backup_file_name ==
  "foo.txt"` (project-relative)

#### Scenario: Successful Edit persists original bytes

- **Given** `/proj/bar.rs` exists with `"fn a() {}\nfn b() {}\n"`
- **When** `EditTool::execute({"file_path": "/proj/bar.rs",
  "old_string": "fn a", "new_string": "fn x"}, &ctx)` runs
- **Then** the file now reads `"fn x() {}\nfn b() {}\n"`
- **And** one snapshot was persisted with
  `content == b"fn a() {}\nfn b() {}\n"` (the bytes Edit
  computed its replacement against)

#### Scenario: Write of net-new file does not snapshot

- **Given** `/proj/new.txt` does not exist
- **When** `WriteTool::execute({"file_path": "/proj/new.txt",
  "content": "hi"}, &ctx)` runs
- **Then** the file is created with contents `"hi"`
- **And** zero `FileHistorySnapshot` entries are present in the
  session JSONL

#### Scenario: Failed Edit does not snapshot

- **Given** `/proj/qux.txt` exists with `"abc"`
- **When** `EditTool::execute({"file_path": "/proj/qux.txt",
  "old_string": "zzz", "new_string": "www"}, &ctx)` runs and
  returns an error because `zzz` is not in the file
- **Then** the file on disk is unchanged
- **And** zero `FileHistorySnapshot` entries are present in the
  session JSONL

#### Scenario: Cancelled Edit does not snapshot

- **Given** `/proj/qux.txt` exists with `"abc"`
- **And** `ctx.cancel` is cancelled before `EditTool::execute`
  completes its precondition checks
- **When** `EditTool::execute` runs
- **Then** it returns a cancellation error
- **And** zero `FileHistorySnapshot` entries are present

### Requirement: cc-query cancel path regression tests lock the interrupt-marker behaviour

`cc-query::engine::tests` MUST include two regression tests that
exercise `run_turn` end-to-end (using the harness from
`fix-engine-stream-mock-harness`) and assert the canonical
interrupt markers reach the session JSONL. The tests close the
coverage gap in the §1 interrupt-marker wiring that landed in
`fix-session-resume-wiring` without regression tests.

#### Scenario: Cancel during tool_use produces all three marker entries

- **Given** a scripted stream yielding `MessageStart`, a
  `tool_use` block, and then a cancellation
- **When** `engine.run_turn(...)` runs with that script
- **Then** the session JSONL contains, in order:
  1. A `Message` entry for the partial assistant turn whose last
     content block contains `cc_session::INTERRUPT_MESSAGE`
  2. A `Message` entry with a synthetic `tool_result` stub whose
     content equals `cc_session::INTERRUPT_MESSAGE_FOR_TOOL_USE`
  3. A canonical interrupt-marker entry produced by
     `Session::append_interrupt_marker(true)`
- **And** `run_turn` returns `Err(CcError::Cancelled)`

#### Scenario: Cancel without tool_use produces two marker entries

- **Given** a scripted stream yielding `MessageStart`, a text
  delta, and then a cancellation
- **When** `engine.run_turn(...)` runs with that script
- **Then** the session JSONL contains:
  1. A `Message` entry ending with `INTERRUPT_MESSAGE`
  2. A canonical interrupt-marker entry from
     `Session::append_interrupt_marker(false)` (plain variant)
- **And** no synthetic `tool_result` stub is present
- **And** `run_turn` returns `Err(CcError::Cancelled)`

### Requirement: Each emitted FileHistorySnapshot points at a distinct on-disk sidecar

`Session::append_file_history_snapshot_for_path` MUST choose a
sidecar filename that does not collide with any existing sidecar
in the session's `file-history/` directory. The chosen name SHALL
follow the `{sha256(relpath)[0..16]}@v{n}` shape (matching TS
`getBackupFileName`), with `n` computed as `max(existing versions
for this relpath) + 1`, or `1` if no prior version exists.

The chosen `n` SHALL be embedded in both the on-disk sidecar
filename and the `FileHistoryBackup.version` field written to the
JSONL, so replay readers can correlate the entry with its backup
file by either channel.

This requirement supersedes the prior contract of always using
`@v1`, which allowed two successful mutations of the same path to
clobber the first one's pre-mutation bytes while still appending
two snapshot entries that both pointed at the same (now-corrupted)
sidecar.

Out of scope for this requirement:

- Pruning of older `@v{n}` entries after a turn boundary. All
  versions accumulate until the session is archived.
- Per-turn `is_snapshot_update = true` differentiation. All
  producer-path snapshots continue to emit with
  `is_snapshot_update = false`; turn-boundary tracking is deferred
  alongside the parent change's §5.

#### Scenario: Two successful Edits of the same file produce two distinct sidecars

- **Given** `/proj/foo.rs` exists with `"a"`
- **And** a session ctx whose sink is a real
  `cc_session::Session`
- **When** `EditTool::execute(...) old="a" new="b"` runs, followed
  by `EditTool::execute(...) old="b" new="c"`
- **Then** `session.file_history_snapshots()` returns exactly two
  entries in append order
- **And** their `backup_file_name` values are distinct, one
  ending in `@v1` and the next in `@v2`
- **And** `session.read_backup({hex}@v1) == b"a"` (the pre-
  Edit #1 bytes)
- **And** `session.read_backup({hex}@v2) == b"b"` (the pre-
  Edit #2 bytes, i.e. the post-Edit #1 state)
- **And** the file on disk equals `"c"`

#### Scenario: Write followed by Edit of the same file produces two distinct sidecars

- **Given** `/proj/foo.txt` exists with `"original"`
- **When** `WriteTool::execute(...) content="middle"` runs,
  followed by `EditTool::execute(...) old="middle" new="final"`
- **Then** two snapshot entries exist, referencing `@v1` and
  `@v2` respectively
- **And** the sidecars hold `"original"` and `"middle"` in that
  order
- **And** the file on disk equals `"final"`

#### Scenario: next_backup_version tolerates unrelated directory entries

- **Given** a backup directory containing `abc@v3`, `def@v9`,
  `.DS_Store`, and a `.tmpABCD` tempfile leftover
- **When** `next_backup_version(dir, "abc")` runs
- **Then** it returns `4` (only `abc@v3` is recognised; unrelated
  prefixes, hidden files, and tempfile leftovers are ignored)

#### Scenario: next_backup_version returns 1 when the directory is missing

- **Given** a backup-directory path that does not exist on disk
- **When** `next_backup_version(path, "abc")` runs
- **Then** it returns `Ok(1)` (NotFound is treated as "no prior
  versions", matching the caller's mkdir-then-scan ordering)

