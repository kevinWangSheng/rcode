use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tracing::debug;

/// A configured hook command.
/// Uses `command: Option<String>` + `#[serde(flatten)]` to tolerate hooks with different
/// schemas in the global settings file (e.g. TS-format plugin hooks that have no `command` field).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HookConfig {
    /// Shell command to execute. None = entry is ignored (different hook format).
    #[serde(default)]
    pub command: Option<String>,
    /// Timeout in seconds (default: 600).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
    /// Absorb unknown fields (e.g. TS plugin hooks with `hooks`, `matcher`, etc.).
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, Value>,
}

impl HookConfig {
    /// Return true only if this entry has an executable command.
    pub fn has_command(&self) -> bool {
        self.command.as_deref().map(|s| !s.is_empty()).unwrap_or(false)
    }
}

/// Settings hooks section — keyed by event name.
/// e.g. `{ "PreToolUse": [{ "command": "my-hook.sh" }] }`
pub type HooksConfig = std::collections::HashMap<String, Vec<HookConfig>>;

/// Outcome of running a hook.
#[derive(Debug)]
pub enum HookOutcome {
    /// Hook ran successfully (exit 0).
    Ok,
    /// Hook requested a blocking stop (exit 2). Contains the message to surface.
    Block(String),
    /// Hook failed (non-zero exit other than 2) — non-blocking.
    ///
    /// `kind` is a machine-readable category (e.g. `"stdin_write"`, `"timeout"`,
    /// `"spawn"`, `"wait"`, `"serialize_input"`); `detail` is a human-readable
    /// message. Keeping the kind separate lets callers log/filter without
    /// parsing prose.
    Failed {
        kind: String,
        detail: String,
    },
}

impl HookOutcome {
    /// Helper: build a `Failed` outcome from a kind + detail.
    pub fn failed(kind: impl Into<String>, detail: impl Into<String>) -> Self {
        HookOutcome::Failed {
            kind: kind.into(),
            detail: detail.into(),
        }
    }
}

/// Input sent to hooks via stdin as JSON.
#[derive(Debug, Serialize)]
pub struct HookInput<'a> {
    pub event: &'a str,
    pub tool_name: &'a str,
    pub tool_input: &'a Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<&'a str>,
}

/// Runs hooks registered for a given event.
#[derive(Debug, Clone)]
pub struct HookRunner {
    config: HooksConfig,
}

impl HookRunner {
    pub fn new(config: HooksConfig) -> Self {
        HookRunner { config }
    }

    pub fn empty() -> Self {
        HookRunner {
            config: HooksConfig::new(),
        }
    }

    /// Execute all hooks registered for `event`.
    /// Returns the first blocking outcome if any hook exits with code 2.
    pub async fn run(&self, event: &str, input: &HookInput<'_>) -> HookOutcome {
        let hooks = match self.config.get(event) {
            Some(h) if !h.is_empty() => h,
            _ => return HookOutcome::Ok,
        };

        let input_json = match serde_json::to_string(input) {
            Ok(j) => j,
            Err(e) => {
                return HookOutcome::failed(
                    "serialize_input",
                    format!("failed to serialize hook input: {e}"),
                );
            }
        };

        for hook in hooks {
            // Skip hooks without a command (e.g. TS plugin-format hooks in global settings)
            let command = match hook.command.as_deref() {
                Some(c) if !c.is_empty() => c.to_string(),
                _ => continue,
            };

            let timeout_secs = hook.timeout.unwrap_or(600);
            debug!("running hook command: {command} (event={event})");

            let result = tokio::time::timeout(
                Duration::from_secs(timeout_secs),
                run_hook_command(&command, &input_json),
            )
            .await;

            match result {
                Err(_) => {
                    return HookOutcome::failed(
                        "timeout",
                        format!("hook timed out after {timeout_secs}s: {command}"),
                    );
                }
                Ok(Err(outcome)) => {
                    return outcome;
                }
                Ok(Ok((exit_code, stdout, _stderr))) => {
                    if exit_code == 2 {
                        // Blocking error — surface message to model
                        return HookOutcome::Block(stdout.trim().to_string());
                    }
                    // Any other non-zero is non-blocking (logged, not blocking)
                    if exit_code != 0 {
                        debug!("hook exited {exit_code}: {command}");
                    }
                }
            }
        }

        HookOutcome::Ok
    }
}

async fn run_hook_command(
    command: &str,
    stdin_data: &str,
) -> Result<(i32, String, String), HookOutcome> {
    let mut child = Command::new("bash")
        .arg("-c")
        .arg(command)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| HookOutcome::failed("spawn", format!("failed to spawn hook: {e}")))?;

    // Write JSON input to stdin. Capture failure in a local flag so the caller
    // learns the real cause (EPIPE, crashed shebang, pipe full) instead of
    // seeing a truncated-JSON error bubble up from the hook's own stderr.
    let mut stdin_err: Option<std::io::Error> = None;
    if let Some(mut stdin) = child.stdin.take() {
        let data = format!("{stdin_data}\n");
        if let Err(e) = stdin.write_all(data.as_bytes()).await {
            stdin_err = Some(e);
        }
        // stdin is dropped here, signaling EOF.
    }

    // Still wait on the child so we reap it cleanly, even when stdin failed.
    let output = child
        .wait_with_output()
        .await
        .map_err(|e| HookOutcome::failed("wait", format!("failed to wait for hook: {e}")))?;

    // If stdin failed, the hook can't have run successfully without its input —
    // skip exit-code interpretation and surface the structured stdin_write
    // failure instead.
    if let Some(e) = stdin_err {
        return Err(HookOutcome::failed(
            "stdin_write",
            format!("failed to write hook stdin: {e}"),
        ));
    }

    let exit_code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    Ok((exit_code, stdout, stderr))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ts_format_hook_has_no_command() {
        // TS-format entry: { "hooks": [...], "async": true } — no top-level "command"
        let json = r#"{"hooks":[{"async":true,"command":"node notify.js","type":"command"}]}"#;
        let cfg: HookConfig = serde_json::from_str(json).unwrap();
        assert!(!cfg.has_command(), "TS-format hook should have no top-level command");
    }

    #[test]
    fn direct_command_hook_is_detected() {
        let json = r#"{"command":"my-hook.sh","timeout":30}"#;
        let cfg: HookConfig = serde_json::from_str(json).unwrap();
        assert!(cfg.has_command());
        assert_eq!(cfg.command.as_deref(), Some("my-hook.sh"));
        assert_eq!(cfg.timeout, Some(30));
    }

    #[test]
    fn hooks_config_mixed_entries() {
        // Simulate the real ~/.claude/settings.json hooks section
        let json = r#"{
            "PreToolUse": [
                {"hooks":[{"async":true,"command":"node notify.js","type":"command"}]},
                {"command":"my-pre-hook.sh"}
            ]
        }"#;
        let config: HooksConfig = serde_json::from_str(json).unwrap();
        let pre = config.get("PreToolUse").unwrap();
        assert_eq!(pre.len(), 2);
        assert!(!pre[0].has_command(), "TS-format entry should be skipped");
        assert!(pre[1].has_command(), "direct command entry should be kept");
    }

    #[tokio::test]
    async fn stdin_write_failure_surfaces_as_structured_failed() {
        // If the child closes stdin immediately and we feed a payload larger
        // than the pipe buffer, the write fails with EPIPE. The runner must
        // surface that as `Failed { kind: "stdin_write", .. }` regardless of
        // the child's own exit code (which would otherwise be 0 here).
        //
        // 256 KiB is well over the typical 64 KiB PIPE_BUF on macOS/Linux.
        let big_tool_input = json!({ "pad": "x".repeat(256 * 1024) });
        let json = r#"{"PreToolUse":[{"command":"exec </dev/null; sleep 0"}]}"#;
        let cfg: HooksConfig = serde_json::from_str(json).unwrap();
        let runner = HookRunner::new(cfg);

        let input = HookInput {
            event: "PreToolUse",
            tool_name: "Bash",
            tool_input: &big_tool_input,
            session_id: None,
        };
        let outcome = runner.run("PreToolUse", &input).await;
        match outcome {
            HookOutcome::Failed { kind, detail } => {
                assert_eq!(
                    kind, "stdin_write",
                    "expected stdin_write kind, got {kind} ({detail})"
                );
            }
            other => panic!("expected Failed stdin_write, got {other:?}"),
        }
    }
}
