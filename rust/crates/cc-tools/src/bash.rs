use async_trait::async_trait;
use cc_core::CcResult;
use serde_json::{json, Value};
use tokio::process::Command;

use crate::{Tool, ToolResult};

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

    fn input_schema(&self) -> Value {
        json!({
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
        })
    }

    fn is_read_only(&self) -> bool {
        false
    }

    async fn execute(&self, input: Value) -> CcResult<ToolResult> {
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
