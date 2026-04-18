use async_trait::async_trait;
use cc_core::CcResult;
use serde_json::{json, Value};
use tokio::process::Command;

use crate::{Tool, ToolInputSchema, ToolResult};
use tokio_util::sync::CancellationToken;

pub struct BashTool;

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "Bash"
    }

    fn description(&self) -> &str {
        "Execute a shell command and return its output. \
         Non-zero exit codes are included in the result (no is_error flag). \
         Use for file operations, running scripts, and system commands."
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

    async fn execute(&self, input: Value, cancel: &CancellationToken) -> CcResult<ToolResult> {
        let command = input["command"]
            .as_str()
            .ok_or_else(|| cc_core::CcError::tool("tool", "missing 'command' field"))?
            .to_string();

        let timeout_ms = input["timeout"].as_u64().unwrap_or(120_000);
        let timeout = std::time::Duration::from_millis(timeout_ms);

        // Spawn the child so we can kill it on cancel. `Command::output()`
        // doesn't give us a handle to do that — it buffers the entire run.
        let mut child = Command::new("bash")
            .arg("-c")
            .arg(&command)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| cc_core::CcError::tool("tool", format!("failed to spawn bash: {e}")))?;

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
                            // Timeout: explicit kill + reap. Don't rely on Drop —
                            // the zombie window on Linux under load leaves the
                            // PID alive from the caller's POV.
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
                // etc.). Explicitly kill + reap the child before returning —
                // Drop is asynchronous and can leave the PID alive from the
                // caller's POV, which breaks "the next bash sees a clean
                // slate" invariants.
                _ = cancel.cancelled() => {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    return Err(cc_core::CcError::tool("tool", "bash execution cancelled"));
                }
            }
        };

        let exit_code = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        // Behavior contract: Bash non-zero exit → include exit code in content, NO is_error flag.
        // Both streams are always included when non-empty — successful commands
        // that write progress to stderr (cargo, make, etc.) would otherwise
        // silently lose that output.
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

        // Trim trailing whitespace
        let content = content.trim_end().to_string();

        Ok(ToolResult::ok(content))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn bash_echo() {
        let tool = BashTool;
        let cancel = CancellationToken::new();
        let result = tool
            .execute(json!({"command": "echo hello"}), &cancel)
            .await
            .unwrap();
        assert!(!result.is_error);
        assert_eq!(result.content, "hello");
    }

    #[tokio::test]
    async fn bash_nonzero_exit_includes_code_no_is_error() {
        let tool = BashTool;
        let cancel = CancellationToken::new();
        let result = tool
            .execute(json!({"command": "exit 42"}), &cancel)
            .await
            .unwrap();
        // Per behavior contract: no is_error for bash failures
        assert!(!result.is_error);
        assert!(result.content.contains("Exit code: 42"));
    }

    #[tokio::test]
    async fn bash_stderr_included_on_failure() {
        let tool = BashTool;
        let cancel = CancellationToken::new();
        let result = tool
            .execute(json!({"command": "echo err >&2 && exit 1"}), &cancel)
            .await
            .unwrap();
        assert!(result.content.contains("STDERR:"));
        assert!(result.content.contains("err"));
    }

    #[tokio::test]
    async fn bash_missing_command_errors() {
        let tool = BashTool;
        let cancel = CancellationToken::new();
        let result = tool.execute(json!({}), &cancel).await;
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
        let cancel = CancellationToken::new();
        let result = tool
            .execute(json!({"command": "echo out && echo err >&2"}), &cancel)
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

    #[tokio::test]
    async fn bash_cancel_explicitly_kills_and_reaps() {
        // Launch a long-running bash that writes its own pid to a sentinel
        // file, then sleeps. Cancel the tool. Once the tool returns, the
        // pid MUST no longer be alive from the OS's POV.
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("pid");
        let pidfile_str = pidfile.to_string_lossy().to_string();

        let tool = BashTool;
        let cancel = CancellationToken::new();
        let cancel2 = cancel.clone();
        tokio::spawn(async move {
            // Give bash time to write the pid.
            tokio::time::sleep(Duration::from_millis(200)).await;
            cancel2.cancel();
        });

        let command = format!("echo $$ > {pidfile_str} && sleep 30");
        let result = tool.execute(json!({"command": command}), &cancel).await;
        assert!(result.is_err(), "expected cancel error, got {:?}", result);

        // Read the pid that bash wrote before sleeping.
        let pid_str = std::fs::read_to_string(&pidfile).unwrap_or_default();
        let pid: i32 = match pid_str.trim().parse() {
            Ok(p) => p,
            Err(_) => return, // If bash didn't even write the pid, nothing to verify.
        };

        // Poll briefly for reaping; on cancel we issue kill+wait so the child
        // should be gone by the time the tool returns, but the immediate
        // parent-child relationship only guarantees the tokio Child was
        // reaped — the bash pid may linger as a zombie on very slow systems.
        // `kill(pid, 0)` returns Err(ESRCH) when the process is gone.
        let mut alive = true;
        for _ in 0..20 {
            // SAFETY: kill(pid, 0) is a no-op signal used to probe liveness.
            let res = unsafe { libc::kill(pid, 0) };
            if res != 0 {
                alive = false;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(
            !alive,
            "pid {pid} still alive after cancel — kill+reap failed"
        );
    }

    #[tokio::test]
    async fn bash_honors_cancel_token() {
        use std::time::Duration;
        let tool = BashTool;
        let cancel = CancellationToken::new();
        let cancel2 = cancel.clone();
        // Cancel after 100ms, while the command is still sleeping.
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cancel2.cancel();
        });
        let result = tool.execute(json!({"command": "sleep 5"}), &cancel).await;
        // Cancelled before the 5s sleep finishes.
        assert!(result.is_err(), "expected cancel error, got {:?}", result);
        assert!(result.unwrap_err().to_string().contains("cancelled"));
    }
}
