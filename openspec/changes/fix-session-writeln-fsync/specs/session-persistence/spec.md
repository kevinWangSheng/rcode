## ADDED Requirements

### Requirement: Per-Turn Durable Append

`cc_session::Session::append` SHALL ensure that each appended JSONL line
is durable on disk before the call returns successfully to the caller.

"Durable" means: the bytes are written through the OS page cache to the
underlying storage's stable layer (fsync semantics). In Rust terms this
is `file.sync_all()` (or `fdatasync` if a benchmark justifies it) on the
append file handle after the `writeln!`.

If the sync fails (disk full, I/O error, read-only mount), `append` SHALL
return `Err(CcError::Io { ... })`. It MUST NOT silently succeed on an
unsynced write.

`Session::write_metadata` SHALL use an atomic tmpfile-plus-rename pattern
so that a crash mid-write cannot leave a truncated `metadata.json` in
place.

#### Scenario: Happy append is durable
- **WHEN** `append(msg)` returns `Ok(())`
- **THEN** a subsequent fresh process reading the transcript at the same
  path sees `msg` as the final JSONL line, regardless of whether the
  original process exited cleanly

#### Scenario: Crash between writes does not lose the prior turn
- **GIVEN** a session that has called `append(msg1)` successfully
- **WHEN** the process is killed with SIGKILL before `append(msg2)` is
  called
- **THEN** a fresh process reopening the transcript sees `msg1` as a
  complete line at position 1

#### Scenario: Disk-full is surfaced, not swallowed
- **GIVEN** a transcript filesystem that is out of space
- **WHEN** `append(msg)` is called
- **THEN** the call returns `Err(CcError::Io { .. })` naming the fsync or
  write failure
- **AND** the caller (query engine) can surface that to the TUI so the
  user sees the failure instead of a silently-lost turn

#### Scenario: Metadata write is atomic
- **GIVEN** `write_metadata(meta)` is called and crashes mid-write
- **WHEN** the next process reads `metadata.json`
- **THEN** it sees either the previous fully-written version or no file,
  never a truncated JSON document
