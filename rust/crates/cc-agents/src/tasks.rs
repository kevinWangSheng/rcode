//! Task type implementations.
//!
//! Each function runs a specific task type as an async future that can be
//! spawned into the TaskRegistry.

use cc_core::{CcError, CcResult, SubAgentRunner};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;
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

/// Run an in-process sub-agent (local_agent task type).
///
/// Executes a single agent turn and returns the output. Wired via the
/// `SubAgentRunner` trait to avoid cc-agents → cc-query circular dependency.
pub async fn run_local_agent(
    prompt: String,
    runner: Arc<dyn SubAgentRunner>,
    cancel: CancellationToken,
) -> CcResult<TaskOutput> {
    let content = runner.run(None, prompt, Vec::new(), cancel).await?;
    Ok(TaskOutput {
        summary: "agent completed".into(),
        content,
    })
}

/// Run an in-process teammate agent (in_process_teammate task type).
///
/// Runs an initial turn, then loops processing messages from the mailbox
/// inbox until the inbox is closed or the task is cancelled. Each inbox
/// message becomes a new user turn.
pub async fn run_in_process_teammate(
    prompt: String,
    runner: Arc<dyn SubAgentRunner>,
    mut inbox: mpsc::Receiver<String>,
    cancel: CancellationToken,
) -> CcResult<TaskOutput> {
    // Run initial turn
    let mut last_output = runner
        .run(None, prompt, Vec::new(), cancel.clone())
        .await?;

    // Process inbox messages until cancelled or closed
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            msg = inbox.recv() => {
                match msg {
                    None => break, // inbox closed (TeamDelete called)
                    Some(message) => {
                        last_output = runner
                            .run(None, message, Vec::new(), cancel.clone())
                            .await?;
                    }
                }
            }
        }
    }

    Ok(TaskOutput {
        summary: "teammate completed".into(),
        content: last_output,
    })
}

/// Remote agent task type: delegate to a remote Claude Code instance via HTTP API.
pub async fn run_remote_agent(
    prompt: String,
    endpoint: String,
    http: reqwest::Client,
    cancel: CancellationToken,
) -> CcResult<TaskOutput> {
    let body = json!({
        "prompt": prompt,
    });

    tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            Err(CcError::Cancelled)
        }
        result = http.post(&endpoint)
            .header("content-type", "application/json")
            .json(&body)
            .send() => {
            let response = result
                .map_err(|e| CcError::Other(format!("remote agent request failed: {e}")))?;

            if !response.status().is_success() {
                return Err(CcError::Other(format!(
                    "remote agent returned status {}",
                    response.status()
                )));
            }

            let text = response.text().await
                .map_err(|e| CcError::Other(format!("remote agent response read failed: {e}")))?;

            Ok(TaskOutput {
                summary: "remote agent completed".into(),
                content: text,
            })
        }
    }
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
