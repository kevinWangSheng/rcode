## ADDED Requirements

### Requirement: Safe-by-Default Command Hook Execution

Command hooks SHALL accept their `command` field as either a string or
an array of strings. The array form SHALL execute via `argv`-style
spawn with no shell interpretation. The string form SHALL require an
explicit sibling flag `unsafe_shell: true` before the runner will
execute it.

Settings-load SHALL reject a string-form `command` that lacks
`unsafe_shell: true` with an error message naming the file path and
hook entry.

At startup, a WARN-level log entry SHALL summarise any `unsafe_shell`
hooks detected, so users can migrate.

#### Scenario: Array form runs without shell
- **GIVEN** a hook entry `command: ["/bin/echo", "hello; rm -rf /"]`
- **WHEN** the hook fires
- **THEN** `/bin/echo` is invoked with a single argument
  `hello; rm -rf /`
- **AND** the semicolon is not interpreted as a shell separator

#### Scenario: String without opt-in is rejected
- **GIVEN** a hook entry `command: "echo hi"` without `unsafe_shell`
- **WHEN** settings are loaded
- **THEN** loading fails with an error naming the file and the hook id

#### Scenario: String with opt-in still works
- **GIVEN** a hook entry `command: "echo hi", unsafe_shell: true`
- **WHEN** the hook fires
- **THEN** it runs via `bash -c "echo hi"` as today
- **AND** a WARN log records that an unsafe-shell hook was used

#### Scenario: Env vars reach both forms
- **WHEN** either hook form fires
- **THEN** `CLAUDE_SESSION_ID` and peers are present in the child
  process environment
