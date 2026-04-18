## ADDED Requirements

### Requirement: Stdin Write Errors Are Visible

The hook command runner SHALL report a stdin-write failure as a
structured `HookRunResult::Failed { kind: "stdin_write", .. }`. It
MUST NOT swallow the error or infer success from the child's exit
code when the input could not be fully delivered.

#### Scenario: Child closes stdin early
- **GIVEN** a hook command that closes stdin before reading
- **WHEN** the runner writes the input JSON
- **THEN** the write fails with `BrokenPipe`
- **AND** the hook result is `HookRunResult::Failed { kind:
  "stdin_write", .. }` carrying the io error detail

#### Scenario: Successful hooks still succeed
- **GIVEN** a well-behaved hook that reads all of stdin and exits 0
- **WHEN** the runner writes the input JSON
- **THEN** the hook result is success as today
