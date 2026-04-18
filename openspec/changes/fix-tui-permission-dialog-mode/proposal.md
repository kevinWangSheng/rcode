## Why

`cc-tui/src/action.rs:178-183` hard-resets the app mode after a
permission dialog decision:

```rust
AppAction::PermissionDeny => {
    ...
    app.mode = AppMode::Streaming;   // hard-coded
}
```

But the dialog may have appeared during streaming **or** after it (a
tool call request arriving just as the final assistant block lands).
Unconditionally writing `Streaming` is wrong in the second case: the
app thinks a turn is ongoing when the engine has already moved on, so
the next keystroke lands in the wrong handler (tool UI instead of
input prompt).

Symmetric issue for `PermissionAllow` / `PermissionAllowAlways` (same
hard-coded mode write).

## What Changes

- Snapshot the pre-dialog mode when `ShowPermission` fires:
  `app.pre_permission_mode = Some(app.mode);`
- On any permission decision, restore `app.mode = app.pre_permission_mode.
  take().unwrap_or(AppMode::Input);`
- If the snapshot is missing (shouldn't happen, but defensive), fall
  back to `AppMode::Input`, which is always a safe resting state.

## Capabilities

### Modified Capabilities
- `tui-permission-dialog`: the dialog MUST restore the pre-dialog app
  mode, not hard-code one.

## Impact

- **Affected code:** `cc-tui/src/app.rs` (new field), `cc-tui/src/
  action.rs` (four decision arms).
- **Risk:** LOW.
