## 1. App State

- [ ] 1.1 Add `last_abort_at: Option<Instant>` to `App`.
- [ ] 1.2 Add `AppAction::ForceQuit`.

## 2. Key Mapping

- [ ] 2.1 In the Ctrl+C branch, inspect `last_abort_at`; within 2 s →
      `ForceQuit`, else `Abort` + set the stamp.

## 3. Shutdown Path

- [ ] 3.1 `ForceQuit` SHALL: disable raw mode, leave alternate screen,
      signal all child tasks, and `exit(130)`.

## 4. UX Hint

- [ ] 4.1 Status-line hint after first Ctrl+C: "press again to force
      quit".

## 5. Tests

- [ ] 5.1 Headless test simulates two Ctrl+C within 2 s and asserts
      `ForceQuit` path taken.
- [ ] 5.2 Single Ctrl+C after 3 s still maps to `Abort`.

## 6. Sign-off

- [ ] 6.1 `cargo test -p cc-tui` + clippy clean.
