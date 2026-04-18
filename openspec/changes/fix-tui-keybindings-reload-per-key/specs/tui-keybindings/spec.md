## ADDED Requirements

### Requirement: Cached Keybinding Map

The TUI SHALL load `~/.claude/keybindings.json` exactly once per
session on startup. It MUST NOT re-read the file for each key event.

A reload SHALL be available on demand via the `/reload-keybindings`
slash command.

#### Scenario: No per-keystroke I/O
- **GIVEN** a running TUI session
- **WHEN** the user types 100 keys in succession
- **THEN** `~/.claude/keybindings.json` is opened at most once (at
  startup)

#### Scenario: Reload via slash command
- **GIVEN** the user edits `~/.claude/keybindings.json` during a
  session
- **WHEN** the user types `/reload-keybindings`
- **THEN** the new map becomes active
- **AND** the TUI reports success (or a parse error referencing the
  file path)

#### Scenario: Malformed file during edit does not wipe cache
- **GIVEN** a valid keybindings cache loaded at startup
- **WHEN** the file is temporarily truncated by an editor save
- **THEN** the cached bindings remain in effect until an explicit
  reload is requested
