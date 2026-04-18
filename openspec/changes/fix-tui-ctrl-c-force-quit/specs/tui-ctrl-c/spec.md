## ADDED Requirements

### Requirement: Double Ctrl+C Force-Quits

The TUI SHALL interpret a second Ctrl+C within 2 seconds of the first
as a force-quit request. It MUST exit with code 130, disabling raw
mode and leaving the alternate screen, regardless of whether the first
abort has completed.

A single Ctrl+C SHALL continue to dispatch a cooperative abort as
today.

#### Scenario: Stuck abort yields to force quit
- **GIVEN** the engine is mid-abort and has not yet released control
- **WHEN** the user presses Ctrl+C again within 2 seconds
- **THEN** the TUI restores terminal state and exits (130)

#### Scenario: Delayed second press is another abort
- **GIVEN** the first Ctrl+C fired 5 seconds ago and the engine has
  since settled
- **WHEN** the user presses Ctrl+C again
- **THEN** the TUI issues a fresh cooperative abort, not a force quit

#### Scenario: Hint is shown after first press
- **WHEN** the user presses Ctrl+C once
- **THEN** the status line shows "press again to force quit" until the
  next event
