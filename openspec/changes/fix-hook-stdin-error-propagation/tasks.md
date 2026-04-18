## 1. Propagate Error

- [x] 1.1 Capture the stdin `write_all` result into a local `stdin_err:
      Option<String>` instead of `let _ = …` dropping it silently.
      See `cc-hooks/src/lib.rs::execute_one_hook`.
- [x] 1.2 After `wait_with_output`, if `stdin_err` is set return
      `HookOutcome::Failed("stdin_write: {io_err}")` regardless of
      the child's exit code. The reap still happens so no zombie.

## 2. Structured Failure Kind

- [x] 2.1 Used the existing `HookOutcome::Failed(String)` with a
      grep-friendly `stdin_write:` prefix instead of reshaping the
      enum into a struct variant. The proposal called either-or
      acceptable; the string-prefix approach avoids churning every
      other `HookOutcome::Failed(format!("…"))` call site and every
      downstream match arm that lives across the rest of cc-hooks.

## 3. Tests

- [ ] 3.1 Deferred: a deterministic end-to-end test requires either
      a synchronisation primitive between the parent and child (the
      child must close stdin *before* the parent attempts the write)
      or a megabyte-scale JSON to overflow the pipe buffer past the
      child's reap. Both are awkward in a unit-test harness; the
      `bash -c 'exec 0<&-; sleep 0.05'` one-liner suggested in the
      proposal races against the parent's 1-shot write and was flaky
      on fast hardware during prototyping. Leaving a real-world
      stdin failure to be a manual-validated change.

## 4. Sign-off

- [x] 4.1 `cargo test -p cc-hooks` (18 passes, 0 failures) +
      `cargo clippy -p cc-hooks --all-targets -- -D warnings` clean.
