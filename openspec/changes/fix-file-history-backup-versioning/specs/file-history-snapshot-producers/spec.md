# Spec — File-history snapshot producers (delta)

## MODIFIED Requirements

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
