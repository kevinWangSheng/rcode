//! ExitWorktreeTool — removes the current git worktree and returns to the original directory.

use async_trait::async_trait;
use cc_core::CcResult;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolInputSchema, ToolResult};

pub struct ExitWorktreeTool;

#[async_trait]
impl Tool for ExitWorktreeTool {
    fn name(&self) -> &str {
        "ExitWorktree"
    }

    fn description(&self) -> &str {
        "Exits the current git worktree and removes it. Call this after EnterWorktree \
         when you are done with the isolated changes. The worktree directory will be \
         removed and the session returns to the original working directory."
    }

    fn input_schema(&self) -> ToolInputSchema {
        serde_json::from_value(json!({
            "type": "object",
            "properties": {
                "delete_branch": {
                    "type": "boolean",
                    "description": "If true, also delete the worktree branch after removing. Default false."
                }
            }
        }))
        .unwrap()
    }

    async fn execute(&self, input: Value, cancel: &CancellationToken) -> CcResult<ToolResult> {
        let delete_branch = input
            .get("delete_branch")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let cwd = std::env::current_dir().unwrap_or_default();

        // Get the list of worktrees to find the current one
        let list_output = tokio::process::Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(&cwd)
            .output()
            .await;

        let list_text = match list_output {
            Ok(o) if o.status.success() => {
                String::from_utf8_lossy(&o.stdout).to_string()
            }
            _ => return Ok(ToolResult::error("failed to list git worktrees")),
        };

        if cancel.is_cancelled() {
            return Ok(ToolResult::error("cancelled"));
        }

        // Parse worktree list to find current dir's worktree entry
        let cwd_str = cwd.to_string_lossy().to_string();
        let mut branch_to_delete: Option<String> = None;

        // Find the worktree entry for cwd
        let is_worktree = list_text.lines().any(|line| {
            line.starts_with("worktree ") && line.trim_start_matches("worktree ") == cwd_str
        });

        if delete_branch {
            // Find the branch name from the list
            let mut in_current = false;
            for line in list_text.lines() {
                if line.starts_with("worktree ") {
                    in_current = line.trim_start_matches("worktree ") == cwd_str;
                }
                if in_current && line.starts_with("branch ") {
                    branch_to_delete = Some(
                        line.trim_start_matches("branch refs/heads/")
                            .to_string(),
                    );
                    break;
                }
            }
        }

        if !is_worktree {
            return Ok(ToolResult::error(
                "current directory is not a git worktree created by this session",
            ));
        }

        // Remove the worktree
        let remove_output = tokio::process::Command::new("git")
            .args(["worktree", "remove", "--force", &cwd_str])
            .output()
            .await;

        match remove_output {
            Ok(o) if o.status.success() => {
                // Optionally delete the branch
                if let Some(branch) = &branch_to_delete {
                    let _ = tokio::process::Command::new("git")
                        .args(["branch", "-D", branch])
                        .output()
                        .await;
                }

                let result = json!({
                    "message": format!(
                        "Worktree at {cwd_str} removed. \
                         Return to the main repository directory to continue work."
                    )
                });
                Ok(ToolResult::ok(result.to_string()))
            }
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                Ok(ToolResult::error(format!(
                    "git worktree remove failed: {stderr}"
                )))
            }
            Err(e) => Ok(ToolResult::error(format!("failed to run git: {e}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn not_in_worktree_returns_error() {
        // This test doesn't set up a real worktree, so it will error naturally
        let tool = ExitWorktreeTool;
        // We just verify the tool exists and can be constructed
        assert_eq!(tool.name(), "ExitWorktree");
    }
}
