## Why

`cc-tui/src/lib.rs:102` reloads the user's keybindings file from disk
on **every** key event:

```rust
Some(Ok(crossterm::event::Event::Key(key))) => {
    let kb = Keybindings::load();          // disk I/O per keystroke
    map_key_event(&key, &kb, &app)
}
```

`Keybindings::load` opens `~/.claude/keybindings.json`, reads it,
parses it, falls back on error. On an SSD that is ~100 µs; on NFS, a
FUSE mount, a laggy home-directory sync client, or a machine under I/O
pressure, it is milliseconds — visible input lag, made worse by
chording (rapid key bursts).

The problem also amplifies any filesystem race the spec file is
involved in: each keystroke is another chance to read a partially-
written file. Coupled with the fact that we silently degrade to
defaults on parse error (see `keybindings.rs:60`), a momentary write
during editor-save can wipe the user's chord map mid-session until the
next keystroke reloads successfully.

## What Changes

- Load keybindings once at startup, store on `App` (or an `Arc` in
  `UpdateContext`).
- Drop the per-keystroke call; `map_key_event` reads from the cached
  copy.
- Add a `/reload-keybindings` slash command that re-reads from disk on
  demand.
- Optional: watch the keybindings file with `notify` and reload
  atomically when it changes. Nice-to-have, not required.

## Capabilities

### Modified Capabilities
- `tui-keybindings`: keybindings are loaded once per session unless
  explicitly reloaded via a slash command.

## Impact

- **Affected code:** `cc-tui/src/lib.rs`, `cc-tui/src/keybindings.rs`,
  `cc-commands` (new slash command).
- **User-visible:** edits to `~/.claude/keybindings.json` no longer take
  effect until the next session or `/reload-keybindings`. This matches
  the behavior of most terminal apps' config files.
- **Risk:** LOW.
