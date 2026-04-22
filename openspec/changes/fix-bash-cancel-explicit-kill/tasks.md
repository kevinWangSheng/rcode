## 1. Explicit Kill + Reap

- [x] 1.1 Own the child handle as `let mut child = spawn()?;`.
- [x] 1.2 On cancel: `child.kill().await.ok(); child.wait().await.ok();`
      before returning the cancelled error.
      QA 2026-04-21: reopened. The current code kills and reaps only the
      top-level `bash -c` process. It does not guarantee teardown of the
      command subtree spawned underneath that shell, so long-lived
      grandchildren can outlive the tool return.
      Fixed 2026-04-21 (round 2): `bash.rs` now calls
      `Command::process_group(0)` at spawn on Unix so the child becomes
      the leader of a fresh process group (pgid = child pid). The
      cancel arm calls a new `kill_process_group(child_pid)` helper
      which issues `libc::kill(-pid, SIGKILL)` before the usual
      `child.kill/wait` pair — SIGKILL now reaches the whole subtree
      (shell + every descendant the shell spawned), not just the
      wrapper shell.
- [x] 1.3 Apply the same pattern to the timeout branch.
      QA 2026-04-21: reopened for the same reason as 1.2. Timeout kills
      and reaps the shell process, but not necessarily the process group
      or descendants launched by the shell.
      Fixed 2026-04-21 (round 2): the timeout branch now calls the same
      `kill_process_group(child_pid)` helper ahead of `child.kill +
      child.wait`, so timed-out commands tear down descendants too.

## 2. Test

- [x] 2.1 Spawn a `sleep 30` via the bash tool, cancel mid-run, assert
      the PID is no longer alive when the tool returns (poll
      `/proc/<pid>` on Linux, `ps -p` elsewhere).
      QA 2026-04-21: reopened. The landed test records `$$`, which is the
      shell PID, not the long-lived child command's PID. It proves the
      wrapper shell dies; it does not prove the actual command tree is
      gone.
      Fixed 2026-04-21 (round 2): replaced with
      `bash_cancel_kills_descendant_process_tree`, which runs
      `sleep 30 & echo $! > pidfile && wait $!` so the pid recorded is
      the backgrounded `sleep` (the actual grandchild), not `$$`. After
      cancel, the test polls `libc::kill(pid, 0)` for up to 2s and
      asserts ESRCH (grandchild reaped). Without the process-group
      kill this assertion fails — the orphaned `sleep` lives the full
      30s.

## 3. Sign-off

- [x] 3.1 `cargo test -p cc-tools` + clippy clean.
      Re-verified 2026-04-21 (round 2): all 123 cc-tools unit tests
      pass including the new grandchild-kill test; `cargo clippy
      --workspace --all-targets -- -D warnings` is clean.

## QA Notes

- 2026-04-21 validation: implementation quality is not sufficient for
  the original contract yet. The fix is enough to avoid leaving the
  direct `tokio::process::Child` unreaped, but not enough to guarantee
  "the next bash sees a clean slate" when the command launches its own
  children.
- 2026-04-21 round-2 validation: landed process-group kill fixes the
  contract. The bash wrapper now spawns every command in its own pgid
  and sends SIGKILL to the entire group on cancel/timeout, so the
  "next bash sees a clean slate" guarantee now holds for commands
  that spawn their own children (backgrounded jobs, long-lived
  subprocesses, etc.). Covered by
  `bash_cancel_kills_descendant_process_tree`.
