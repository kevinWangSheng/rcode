## 1. Startup Cache

- [x] 1.1 Call `Keybindings::load` once at `run_tui` init, store on
      `App` (or a shared `Arc<Keybindings>`).
- [x] 1.2 Remove the `Keybindings::load` call from the key-event branch
      in `lib.rs`.

## 2. Reload Command

- [x] 2.1 Add `/reload-keybindings` builtin in `cc-commands` that
      re-runs `Keybindings::load` and swaps it on `App`.
      Fixed 2026-04-18 — `cc-commands` was absorbed into `cc-tui`
      (Phase 2 Decision 3); new `Builtin::ReloadKeybindings` returns
      `CommandOutcome::ReloadKeybindings`, and `update()` dispatches
      `AppAction::ReloadKeybindings` which calls `Keybindings::try_load`
      and swaps `App::keybindings` in place.
- [x] 2.2 Report to the TUI (toast / status line) whether the reload
      succeeded and which keys changed.
      Fixed 2026-04-18 — success sets `app.status_hint` to
      "Reloaded N keybindings" and records a transcript `SystemNotice`;
      failure sets "Reload failed: <path>: <err>" and preserves the
      previously-cached map (malformed-file scenario).

## 3. Optional: File Watcher

- [ ] 3.1 Evaluate `notify` integration so saves in the user's editor
      pick up automatically. Feature-gate if noisy.

## 4. Tests

- [x] 4.1 Unit test that `handle_key_event` uses the cached bindings
      after the first load.
- [x] 4.2 Test that `/reload-keybindings` with a modified file swaps
      the active map.
      Fixed 2026-04-18 — added four tests in `cc-tui/src/action.rs`
      (`reload_keybindings_swaps_active_map_and_shows_toast`,
      `reload_keybindings_with_malformed_file_preserves_cache_and_reports_error`,
      `reload_keybindings_action_runs_and_records_notice`,
      `slash_reload_keybindings_dispatches_reload_not_submit_to_engine`)
      plus registry coverage in `cc-tui/src/commands.rs`
      (`reload_keybindings_command_returns_reload_outcome`,
      `help_advertises_reload_keybindings`).

## 5. Sign-off

- [x] 5.1 `cargo test -p cc-tui -p cc-commands` + clippy clean.
