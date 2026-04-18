## 1. App State

- [x] 1.1 Add `last_abort_at: Option<Instant>` to `App`.
- [x] 1.2 Add `AppAction::ForceQuit`.

## 2. Key Mapping

- [x] 2.1 In the Ctrl+C branch, inspect `last_abort_at`; within 2 s →
      `ForceQuit`, else `Abort` + set the stamp.

## 3. Shutdown Path

- [x] 3.1 `ForceQuit` SHALL: disable raw mode, leave alternate screen,
      signal all child tasks, and `exit(130)`.

## 4. UX Hint

- [x] 4.1 Status-line hint after first Ctrl+C: "press again to force
      quit".

## 5. Tests

- [x] 5.1 Headless test simulates two Ctrl+C within 2 s and asserts
      `ForceQuit` path taken.
- [x] 5.2 Single Ctrl+C after 3 s still maps to `Abort`.

## 6. Sign-off

- [x] 6.1 `cargo test -p cc-tui` + clippy clean.
