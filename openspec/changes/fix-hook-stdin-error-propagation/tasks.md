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

- [x] 3.1 Deterministic E2E test landed.
      Fixed 2026-04-18: `tests::stdin_write_failure_surfaces_as_structured_failure`
      in `cc-hooks/src/lib.rs`. The recipe uses the **pipe-buffer overflow**
      approach rather than the synchronisation-primitive one — the child
      runs `exec 0<&-; exit 0` (close stdin immediately) and the parent
      tries to `write_all` a 2 MiB payload via `HookInput.message`. Because
      2 MiB is well beyond any realistic kernel pipe buffer (macOS 16–64
      KiB, Linux 64 KiB), the writer ends up blocked on `write_all`, at
      which point the closed reader end forces `BrokenPipe`. No `sleep`s,
      no races, no files — purely driven by the kernel pipe size.
      The test asserts the failure is tagged with the `stdin_write:`
      prefix and not interpreted as a block. Ran locally 5× back-to-back
      with zero flakes.

## 4. Sign-off

- [x] 4.1 `cargo test -p cc-hooks` (18 passes, 0 failures) +
      `cargo clippy -p cc-hooks --all-targets -- -D warnings` clean.
