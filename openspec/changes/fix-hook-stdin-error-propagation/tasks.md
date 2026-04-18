## 1. Propagate Error

- [ ] 1.1 Change the stdin write to capture the `Result`; on error,
      set a local `stdin_err` flag.
- [ ] 1.2 After `wait`, if `stdin_err` is set, return
      `HookRunResult::Failed { kind: "stdin_write", .. }` regardless of
      child exit code.

## 2. Structured Failure Kind

- [ ] 2.1 Add a `stdin_write` variant (or reuse a general `Io` kind) in
      `HookRunResult::Failed::kind`.

## 3. Tests

- [ ] 3.1 Launch a shell one-liner that closes stdin immediately
      (`bash -c 'exec </dev/null; sleep 0'`); assert
      `HookRunResult::Failed` with `stdin_write` is produced.

## 4. Sign-off

- [ ] 4.1 `cargo test -p cc-hooks` + clippy clean.
