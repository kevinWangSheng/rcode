## Why

`cc-tui/src/action.rs` only maps Ctrl+C to `AppAction::Abort`. There is
no second-press escalation. If the abort itself hangs — the engine is
stuck inside a slow syscall, a tool cancel deadlocks, a hook refuses to
exit — the user has no way out except killing the terminal. Every other
terminal TUI (htop, less, git log) treats a second Ctrl+C as "no really,
quit now", and users expect the same here.

## What Changes

- Track `last_abort_at: Option<Instant>` on `App`.
- On Ctrl+C:
  - if `last_abort_at` is `None` or older than 2 s → set it and
    dispatch `AppAction::Abort` as today.
  - if `last_abort_at` is within 2 s → dispatch
    `AppAction::ForceQuit`: restore terminal, send SIGTERM to any
    child task, `exit(130)`.
- Show a subtle hint in the status line after the first Ctrl+C ("press
  again to force quit") so the escalation is discoverable.

## Capabilities

### Modified Capabilities
- `tui-ctrl-c`: a second Ctrl+C within 2 s MUST force-quit the TUI
  even if the first abort is still in flight.

## Impact

- **Affected code:** `cc-tui/src/action.rs`, `cc-tui/src/app.rs`,
  `cc-tui/src/lib.rs` (shutdown path).
- **Risk:** LOW, provided the force-quit path restores terminal state
  cleanly (disables raw mode, clears alternate screen).
