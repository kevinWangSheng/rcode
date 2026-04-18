## ADDED Requirements

### Requirement: Atomic, Mode-Preserving Edit

The `Edit` tool SHALL write its output via a same-directory tempfile
followed by `rename(tmp, target)`. It MUST NOT use the `open(truncate) +
write` pattern directly on the target path.

The tempfile SHALL be flushed and `sync_all`'d before the rename.

The pre-existing file mode of the target SHALL be captured and applied
to the tempfile before the rename, so a `chmod +x` script does not lose
its executable bit through an edit.

#### Scenario: Crash during Edit
- **GIVEN** an Edit in progress on `/tmp/x`
- **WHEN** the process is killed with SIGKILL before the rename
- **THEN** `/tmp/x` remains in its pre-edit state
- **AND** is never observed as empty or truncated

#### Scenario: Concurrent Edits
- **GIVEN** two Edit calls targeting the same file
- **WHEN** both run to completion
- **THEN** the final file content is one of the two edits' results
- **AND** neither call observed a truncated file in the interim

#### Scenario: Executable mode preserved
- **GIVEN** a file `script.sh` with mode `0o755`
- **WHEN** `Edit` is invoked to change its contents
- **THEN** after the edit the file mode is still `0o755`
