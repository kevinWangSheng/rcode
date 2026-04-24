use async_trait::async_trait;
use cc_core::CcResult;
use serde_json::{json, Value};
use tokio::process::Command;

use crate::{Tool, ToolContext, ToolInputSchema, ToolResult};

pub struct BashTool;

/// Send SIGKILL to the child's process group so any descendants the
/// wrapper shell spawned (commands it `exec`ed, background jobs, etc.)
/// are torn down too. On Unix we paired this with `Command::process_group(0)`
/// at spawn time, so the child's PID equals its process-group id.
/// On non-Unix platforms this is a no-op; the direct `child.kill()`
/// that callers still invoke is the best we can do there.
#[cfg(unix)]
fn kill_process_group(pid: Option<u32>) {
    if let Some(pid) = pid {
        // SAFETY: `kill` is a POSIX syscall with no preconditions beyond
        // the arguments being in range. A negative `pid` targets the
        // process group whose id equals `-pid` (see kill(2)).
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
}

#[cfg(not(unix))]
fn kill_process_group(_pid: Option<u32>) {
    // Process groups are a Unix concept; fall back to the per-child
    // kill that the caller still issues.
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "Bash"
    }

    fn description(&self) -> &str {
        "Execute a shell command and return its output. \
         Non-zero exit codes flag the result as is_error (and include the \
         exit code in the body). Use for file operations, running scripts, \
         and system commands."
    }

    fn input_schema(&self) -> ToolInputSchema {
        serde_json::from_value(json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                },
                "timeout": {
                    "type": "number",
                    "description": "Timeout in milliseconds (default: 120000)"
                },
                "description": {
                    "type": "string",
                    "description": "Brief description of what this command does"
                }
            },
            "required": ["command"]
        }))
        .unwrap()
    }

    fn is_read_only(&self) -> bool {
        false
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> CcResult<ToolResult> {
        let command = input["command"]
            .as_str()
            .ok_or_else(|| cc_core::CcError::tool("tool", "missing 'command' field"))?
            .to_string();

        let timeout_ms = input["timeout"].as_u64().unwrap_or(120_000);
        let timeout = std::time::Duration::from_millis(timeout_ms);

        // Spawn the child so we can kill it on cancel. `Command::output()`
        // doesn't give us a handle to do that — it buffers the entire run.
        //
        // On Unix we also put the child in its own process group via
        // `process_group(0)` so that on cancel/timeout we can send
        // SIGKILL to the *whole group* (pgid = child PID). Without
        // this, `child.kill()` only reaps the wrapper shell and any
        // long-lived descendants bash spawned (`sleep 30`, background
        // jobs, etc.) are reparented to init and keep running — which
        // breaks the "next bash sees a clean slate" invariant this fix
        // is supposed to guarantee. See openspec
        // `fix-bash-cancel-explicit-kill` §1.
        let mut cmd = Command::new("bash");
        cmd.arg("-c")
            .arg(&command)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        #[cfg(unix)]
        {
            // `tokio::process::Command::process_group` is the inherent
            // wrapper over `std::os::unix::process::CommandExt::process_group`,
            // so no trait import is needed. Pgid `0` creates a new group
            // with the child as leader, giving us a stable target for
            // `killpg` on cancel/timeout.
            cmd.process_group(0);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| cc_core::CcError::tool("tool", format!("failed to spawn bash: {e}")))?;
        // Capture the PID up-front: `child.id()` returns `None` after
        // `wait()` consumes the exit status, and we may need the PID
        // in the cancel/timeout arms below which run after wait.
        let child_pid = child.id();

        let output = {
            // Drive wait + cancel in a separate scope so the borrow of `child`
            // by wait() ends before we reach the post-select cleanup arms.
            let run = async {
                let status = child.wait().await?;
                // Drain stdout/stderr after the child exits. Tokio pipes are
                // buffered so this won't deadlock — they're fully written by
                // the time wait() returns.
                use tokio::io::AsyncReadExt;
                let mut stdout_bytes = Vec::new();
                if let Some(mut o) = child.stdout.take() {
                    let _ = o.read_to_end(&mut stdout_bytes).await;
                }
                let mut stderr_bytes = Vec::new();
                if let Some(mut e) = child.stderr.take() {
                    let _ = e.read_to_end(&mut stderr_bytes).await;
                }
                Ok::<_, std::io::Error>(std::process::Output {
                    status,
                    stdout: stdout_bytes,
                    stderr: stderr_bytes,
                })
            };

            tokio::select! {
                result = tokio::time::timeout(timeout, run) => {
                    match result {
                        Err(_) => {
                            // Timeout: kill the whole process group (shell +
                            // descendants) then reap the direct child. A plain
                            // `child.kill()` only signals the wrapper shell
                            // and leaves its children running.
                            kill_process_group(child_pid);
                            let _ = child.kill().await;
                            let _ = child.wait().await;
                            return Err(cc_core::CcError::tool(
                                "tool",
                                format!("command timed out after {timeout_ms}ms"),
                            ));
                        }
                        Ok(Err(e)) => {
                            return Err(cc_core::CcError::tool("tool", format!("bash i/o failed: {e}")));
                        }
                        Ok(Ok(out)) => out,
                    }
                }
                // Cancel token fires (Ctrl+C, permission denied mid-flight,
                // etc.). Kill the entire process group + reap — Drop is
                // asynchronous and only reaps the shell, which leaves
                // grandchild commands (e.g. `sleep 30`) orphaned and alive,
                // breaking "the next bash sees a clean slate".
                _ = ctx.cancel.cancelled() => {
                    kill_process_group(child_pid);
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    return Err(cc_core::CcError::tool("tool", "bash execution cancelled"));
                }
            }
        };

        let exit_code = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        // Exit-code-based error signalling: a non-zero exit flips
        // `is_error=true` so the TUI renders `✗` instead of `✓`
        // (2026-04-24 critique P0 #3). Successful commands that stream
        // progress to stderr (cargo, make, etc.) still exit 0, so they
        // surface as success — the earlier contract of "never flag
        // is_error" was over-broad and hid genuine failures behind a
        // green tick.
        let content = if exit_code == 0 {
            if stdout.is_empty() && stderr.is_empty() {
                String::new()
            } else if stdout.is_empty() {
                stderr
            } else if stderr.is_empty() {
                stdout
            } else {
                format!("{stdout}\nSTDERR:\n{stderr}")
            }
        } else {
            let mut parts = Vec::new();
            if !stdout.is_empty() {
                parts.push(stdout);
            }
            if !stderr.is_empty() {
                parts.push(format!("STDERR:\n{stderr}"));
            }
            parts.push(format!("Exit code: {exit_code}"));
            parts.join("\n")
        };

        let content = content.trim_end().to_string();

        if exit_code == 0 {
            Ok(ToolResult::ok(content))
        } else {
            Ok(ToolResult::error(content))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn bash_echo() {
        let tool = BashTool;
        let ctx = ToolContext::for_test_bare(CancellationToken::new());
        let result = tool
            .execute(json!({"command": "echo hello"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        assert_eq!(result.content, "hello");
    }

    #[tokio::test]
    async fn bash_nonzero_exit_flags_is_error_and_includes_code() {
        let tool = BashTool;
        let ctx = ToolContext::for_test_bare(CancellationToken::new());
        let result = tool
            .execute(json!({"command": "exit 42"}), &ctx)
            .await
            .unwrap();
        // 2026-04-24 critique P0 #3: non-zero exit → is_error=true so
        // the TUI renders ✗ instead of a misleading green ✓.
        assert!(result.is_error);
        assert!(result.content.contains("Exit code: 42"));
    }

    #[tokio::test]
    async fn bash_stderr_only_on_success_is_not_error() {
        // cargo/make pattern: non-empty stderr with exit 0 still reads
        // as success. Guards the flipside of the P0 #3 fix.
        let tool = BashTool;
        let ctx = ToolContext::for_test_bare(CancellationToken::new());
        let result = tool
            .execute(json!({"command": "echo progress >&2; exit 0"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("progress"));
    }

    #[tokio::test]
    async fn bash_stderr_included_on_failure() {
        let tool = BashTool;
        let ctx = ToolContext::for_test_bare(CancellationToken::new());
        let result = tool
            .execute(json!({"command": "echo err >&2 && exit 1"}), &ctx)
            .await
            .unwrap();
        assert!(result.content.contains("STDERR:"));
        assert!(result.content.contains("err"));
    }

    #[tokio::test]
    async fn bash_missing_command_errors() {
        let tool = BashTool;
        let ctx = ToolContext::for_test_bare(CancellationToken::new());
        let result = tool.execute(json!({}), &ctx).await;
        assert!(result.is_err());
    }

    #[test]
    fn bash_is_not_read_only() {
        assert!(!BashTool.is_read_only());
    }

    #[tokio::test]
    async fn bash_success_includes_both_stdout_and_stderr() {
        // Regression: successful commands that write to both streams (cargo,
        // make, etc.) used to drop stderr. Both must be surfaced now.
        let tool = BashTool;
        let ctx = ToolContext::for_test_bare(CancellationToken::new());
        let result = tool
            .execute(json!({"command": "echo out && echo err >&2"}), &ctx)
            .await
            .unwrap();
        assert!(
            result.content.contains("out"),
            "stdout missing: {:?}",
            result.content
        );
        assert!(
            result.content.contains("STDERR:"),
            "stderr marker missing: {:?}",
            result.content
        );
        assert!(
            result.content.contains("err"),
            "stderr body missing: {:?}",
            result.content
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bash_cancel_kills_descendant_process_tree() {
        // Regression for the QA reopen of `fix-bash-cancel-explicit-kill`:
        // plain `child.kill()` only reaps the wrapper `bash -c` shell. If
        // that shell spawned a long-lived command (`sleep 30`, a backgrounded
        // job, …), the command is reparented to init and keeps running —
        // which breaks the "next bash sees a clean slate" invariant.
        //
        // This test records the GRANDCHILD's pid (the `sleep 30`), not `$$`
        // (the shell), cancels mid-run, and asserts the grandchild is gone
        // by the time the tool returns. It is the specific assertion the
        // previous `echo $$ > pidfile` test did not make.
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("child_pid");
        let pidfile_str = pidfile.to_string_lossy().to_string();

        let tool = BashTool;
        let token = CancellationToken::new();
        let ctx = ToolContext::for_test_bare(token.clone());
        let token2 = token.clone();
        tokio::spawn(async move {
            // Give bash time to launch sleep and write the pid.
            tokio::time::sleep(Duration::from_millis(250)).await;
            token2.cancel();
        });

        // `sleep 30 &` backgrounds the sleep; `$!` is the backgrounded
        // pid (the actual long-lived grandchild). `wait $!` blocks the
        // shell until sleep exits, which it never does before cancel.
        // Note: without process-group kill, SIGKILL'ing the shell
        // leaves the `sleep 30` orphaned and still running — which is
        // what this test guards against.
        let command = format!("sleep 30 & echo $! > {pidfile_str} && wait $!");
        let result = tool.execute(json!({"command": command}), &ctx).await;
        assert!(result.is_err(), "expected cancel error, got {:?}", result);

        let pid_str = std::fs::read_to_string(&pidfile).unwrap_or_default();
        let pid: i32 = pid_str
            .trim()
            .parse()
            .expect("background sleep pid should be recorded before cancel");
        assert!(pid > 0, "pid must be positive, got {pid}");

        // The grandchild can linger briefly after cancel returns on slow
        // systems (zombie reap window). Poll with a short budget — if it
        // outlives this window we've leaked a descendant.
        let mut alive = true;
        for _ in 0..40 {
            // SAFETY: kill(pid, 0) is a no-op signal used only to probe
            // liveness. It returns 0 while the pid is alive, -1 once
            // it's gone (errno = ESRCH).
            let res = unsafe { libc::kill(pid, 0) };
            if res != 0 {
                alive = false;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(
            !alive,
            "grandchild pid {pid} (the `sleep 30`) still alive after cancel — \
             process-group kill failed to reach the descendant subtree"
        );
    }

    #[tokio::test]
    async fn bash_honors_cancel_token() {
        use std::time::Duration;
        let tool = BashTool;
        let token = CancellationToken::new();
        let ctx = ToolContext::for_test_bare(token.clone());
        let token2 = token.clone();
        // Cancel after 100ms, while the command is still sleeping.
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            token2.cancel();
        });
        let result = tool.execute(json!({"command": "sleep 5"}), &ctx).await;
        // Cancelled before the 5s sleep finishes.
        assert!(result.is_err(), "expected cancel error, got {:?}", result);
        assert!(result.unwrap_err().to_string().contains("cancelled"));
    }
}
