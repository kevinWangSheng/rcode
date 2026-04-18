## Why

`cc-tools/src/bash.rs:75-80` handles cancel by returning early and
relying on tokio's `Child` implementation to kill the process on drop:

```rust
_ = cancel.cancelled() => {
    // tokio's Child kills on Drop; returning is enough.
    return Err(cc_core::CcError::tool("tool", "bash execution cancelled"));
}
```

That is mostly correct, but two edges bite:

1. **Drop-order race.** The child handle is owned by the `tokio::select!`
   future. Returning from the select arm drops the future, which drops
   the child, which *schedules* a kill. The kill itself is asynchronous;
   the caller may spawn the next bash before the previous one is
   actually gone.
2. **Zombie window.** Without an explicit `.wait().await` after the
   kill, the child sits in `zombie` state until the reaper polls. On
   macOS this is fine; on Linux under heavy load it can matter.

## What Changes

- Keep the child handle ownership explicit: `let mut child = Command::
  ...spawn()?;`
- On cancel, call `child.kill().await.ok();` then `child.wait().await.ok();`
  before returning. This makes the kill synchronous from the caller's
  perspective and reaps the zombie.

## Capabilities

### Modified Capabilities
- `tool-bash-cancel`: Bash cancel MUST explicitly kill and reap the
  child before returning.

## Impact

- **Affected code:** `cc-tools/src/bash.rs`.
- **Risk:** LOW.
