## 1. Explicit Kill + Reap

- [ ] 1.1 Own the child handle as `let mut child = spawn()?;`.
- [ ] 1.2 On cancel: `child.kill().await.ok(); child.wait().await.ok();`
      before returning the cancelled error.
- [ ] 1.3 Apply the same pattern to the timeout branch.

## 2. Test

- [ ] 2.1 Spawn a `sleep 30` via the bash tool, cancel mid-run, assert
      the PID is no longer alive when the tool returns (poll
      `/proc/<pid>` on Linux, `ps -p` elsewhere).

## 3. Sign-off

- [ ] 3.1 `cargo test -p cc-tools` + clippy clean.
