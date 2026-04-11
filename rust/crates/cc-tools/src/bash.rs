use async_trait::async_trait;
use cc_core::CcResult;
use serde_json::{json, Value};
use tokio::process::Command;

use crate::{Tool, ToolResult, ToolInputSchema};
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
        })).unwrap()
    }

    fn is_read_only(&self) -> bool {
        false
    }

    async fn execute(&self, input: Value, _cancel: &CancellationToken) -> CcResult<ToolResult> {
        let command = input["command"]
            .as_str()
            .ok_or_else(|| cc_core::CcError::tool("tool", "missing 'command' field"))?
            .to_string();

        let timeout_ms = input["timeout"].as_u64().unwrap_or(120_000);
        let timeout = std::time::Duration::from_millis(timeout_ms);

        let output = tokio::time::timeout(
            timeout,
            Command::new("bash")
                .arg("-c")
                .arg(&command)
                .output(),
        )
        .await
        .map_err(|_| cc_core::CcError::tool("tool", format!("command timed out after {timeout_ms}ms")))?
        .map_err(|e| cc_core::CcError::tool("tool", format!("failed to spawn bash: {e}")))?;

        let exit_code = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        // Behavior contract: Bash non-zero exit → include exit code in content, NO is_error flag.
        let content = if exit_code == 0 {
            if stdout.is_empty() && !stderr.is_empty() {
                stderr
            } else {
                stdout
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
        let result = tool.execute(json!({"command": "echo hello"}), &cancel).await.unwrap();
        assert!(!result.is_error);
        assert_eq!(result.content, "hello");
    }

    #[tokio::test]
    async fn bash_nonzero_exit_includes_code_no_is_error() {
        let tool = BashTool;
        let cancel = CancellationToken::new();
        let result = tool.execute(json!({"command": "exit 42"}), &cancel).await.unwrap();
        // Per behavior contract: no is_error for bash failures
        assert!(!result.is_error);
        assert!(result.content.contains("Exit code: 42"));
    }

    #[tokio::test]
    async fn bash_stderr_included_on_failure() {
        let tool = BashTool;
        let cancel = CancellationToken::new();
        let result = tool.execute(json!({"command": "echo err >&2 && exit 1"}), &cancel).await.unwrap();
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
}
