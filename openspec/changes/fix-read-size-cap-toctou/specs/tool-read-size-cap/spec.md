## ADDED Requirements

### Requirement: Atomic Size Cap for Read Tool

The `Read` tool SHALL enforce its `MAX_FILE_BYTES` cap using the same
file descriptor that it subsequently reads from. It MUST NOT stat the
path and then reopen the path for the read, because that pattern is
vulnerable to a TOCTOU symlink swap that defeats the cap.

Concretely: one `tokio::fs::File::open(path)` call, then
`file.metadata().await` on that fd, then read the content through the
same fd.

#### Scenario: Cap enforced under concurrent swap
- **GIVEN** a 10 GB file reachable behind an attacker-controlled symlink
- **AND** the symlink is relinked repeatedly during the read
- **WHEN** `Read` is invoked with `MAX_FILE_BYTES = 50 MB`
- **THEN** either the read returns the content of a ≤ 50 MB target, or
  it returns the cap-exceeded error
- **AND** memory usage during the call stays bounded by the cap

#### Scenario: Normal read unchanged
- **GIVEN** a 1 MB regular file
- **WHEN** `Read` is invoked
- **THEN** the tool returns the file's content
- **AND** there is exactly one `open` syscall against the path
