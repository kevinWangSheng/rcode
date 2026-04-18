## ADDED Requirements

### Requirement: Preserve File Mode on Overwrite

The `Write` tool SHALL preserve the pre-existing mode of the target
file when overwriting. It MUST NOT silently downgrade a `0o755`
executable to `0o644` or any other umask-default.

New files (no pre-existing target) SHALL use the umask-default as
today.

#### Scenario: Executable preserved
- **GIVEN** a file `script.sh` with mode `0o755`
- **WHEN** `Write` is invoked to overwrite it
- **THEN** the file mode after the write is still `0o755`

#### Scenario: New file uses umask
- **GIVEN** no file at the target path
- **WHEN** `Write` is invoked
- **THEN** the newly-created file has the umask-default mode
