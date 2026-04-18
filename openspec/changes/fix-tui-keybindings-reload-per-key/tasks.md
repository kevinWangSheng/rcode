## 1. Startup Cache

- [ ] 1.1 Call `Keybindings::load` once at `run_tui` init, store on
      `App` (or a shared `Arc<Keybindings>`).
- [ ] 1.2 Remove the `Keybindings::load` call from the key-event branch
      in `lib.rs`.

## 2. Reload Command

- [ ] 2.1 Add `/reload-keybindings` builtin in `cc-commands` that
      re-runs `Keybindings::load` and swaps it on `App`.
- [ ] 2.2 Report to the TUI (toast / status line) whether the reload
      succeeded and which keys changed.

## 3. Optional: File Watcher

- [ ] 3.1 Evaluate `notify` integration so saves in the user's editor
      pick up automatically. Feature-gate if noisy.

## 4. Tests

- [ ] 4.1 Unit test that `handle_key_event` uses the cached bindings
      after the first load.
- [ ] 4.2 Test that `/reload-keybindings` with a modified file swaps
      the active map.

## 5. Sign-off

- [ ] 5.1 `cargo test -p cc-tui -p cc-commands` + clippy clean.
