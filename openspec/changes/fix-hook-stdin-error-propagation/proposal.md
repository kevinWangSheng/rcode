## Why

`cc-hooks::run_command_hook` feeds the hook JSON via stdin:

```rust
if let Some(mut stdin) = child.stdin.take() {
    let data = format!("{input_json}\n");
    let _ = stdin.write_all(data.as_bytes()).await;   // error swallowed
}
```

`let _ =` drops any `Err` from the write. If the child closed stdin
early (crashed, misbehaving shebang) or the pipe filled, the hook
receives a truncated JSON and its own parser fails. The hook then
exits non-zero with a confusing "unexpected EOF" error in its stderr,
which cc-hooks reports as a blocking failure if `asyncRewake: true`.

The actual failure — we never finished writing stdin — is invisible.
Debugging this class of bug requires reading the hook's own stderr,
which is awkward.

## What Changes

- Propagate the write error. On failure, surface a
  `HookRunResult::Failed { kind: "stdin_write", detail: <io error> }`
  so the engine's hook-status logging records the actual cause.
- Still attempt `wait` on the child so we reap it cleanly, but skip
  exit-code interpretation if the stdin phase failed — the hook can't
  have run successfully without its input.

## Capabilities

### Modified Capabilities
- `hook-stdin-feed`: stdin write failures MUST propagate as a
  structured hook failure, not be silently dropped.

## Impact

- **Affected code:** `cc-hooks/src/...` (the command runner),
  possibly `cc_core::hook::HookRunResult` for a new failure reason.
- **Risk:** LOW.
