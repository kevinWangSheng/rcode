//! Task type implementations.
//!
//! Each function runs a specific task type as an async future that can be
//! spawned into the TaskRegistry.

use cc_core::{CcError, CcResult};
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

use crate::TaskOutput;

/// Run a shell command in the background (local_bash task type).
pub async fn run_local_bash(
    command: String,
    cwd: PathBuf,
    cancel: CancellationToken,
) -> CcResult<TaskOutput> {
    let child = tokio::process::Command::new("bash")
        .arg("-c")
        .arg(&command)
        .current_dir(&cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| CcError::tool("local_bash", format!("failed to spawn: {e}")))?;

    tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            // Child process cleanup is handled by Drop
            Err(CcError::Cancelled)
        }
        result = child.wait_with_output() => {
            let output = result.map_err(|e| CcError::tool("local_bash", e.to_string()))?;
            let exit_code = output.status.code().unwrap_or(-1);
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let content = if stderr.is_empty() {
                stdout
            } else {
                format!("{stdout}\nSTDERR:\n{stderr}")
            };
            Ok(TaskOutput {
                summary: format!("exit {exit_code}"),
                content,
            })
        }
    }
}

/// Placeholder for local_agent task type.
/// Full implementation requires a reference to QueryEngine which creates
/// a circular dependency — will be wired via trait object in Layer 6.
pub async fn run_local_agent(
    _prompt: String,
    _cancel: CancellationToken,
) -> CcResult<TaskOutput> {
    Err(CcError::Other(
        "local_agent not yet implemented".to_string(),
    ))
}

/// Placeholder for in_process_teammate task type.
pub async fn run_in_process_teammate(
    _prompt: String,
    _cancel: CancellationToken,
) -> CcResult<TaskOutput> {
    Err(CcError::Other(
        "in_process_teammate not yet implemented".to_string(),
    ))
}

/// Placeholder for remote_agent task type.
pub async fn run_remote_agent(
    _prompt: String,
    _endpoint: String,
    _cancel: CancellationToken,
) -> CcResult<TaskOutput> {
    Err(CcError::Other(
        "remote_agent not yet implemented".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_bash_runs_command() {
        let cancel = CancellationToken::new();
        let result = run_local_bash(
            "echo hello".into(),
            std::env::current_dir().unwrap(),
            cancel,
        )
        .await
        .unwrap();
        assert_eq!(result.summary, "exit 0");
        assert!(result.content.contains("hello"));
    }

    #[tokio::test]
    async fn local_bash_captures_exit_code() {
        let cancel = CancellationToken::new();
        let result = run_local_bash(
            "exit 42".into(),
            std::env::current_dir().unwrap(),
            cancel,
        )
        .await
        .unwrap();
        assert_eq!(result.summary, "exit 42");
    }

    #[tokio::test]
    async fn local_bash_cancellation() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = run_local_bash(
            "sleep 60".into(),
            std::env::current_dir().unwrap(),
            cancel,
        )
        .await;
        assert!(matches!(result, Err(CcError::Cancelled)));
    }
}
