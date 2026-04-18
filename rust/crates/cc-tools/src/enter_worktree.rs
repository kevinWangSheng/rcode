//! EnterWorktreeTool — creates an isolated git worktree and switches into it.
//!
//! Creates a new git worktree from the current repo, generates a random name
//! if none is provided, and returns the worktree path and branch.

use async_trait::async_trait;
use cc_core::CcResult;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolInputSchema, ToolResult};

pub struct EnterWorktreeTool;

/// Validate a worktree slug: letters, digits, dots, underscores, dashes; max 64 chars.
fn validate_slug(s: &str) -> Result<(), String> {
    if s.is_empty() {
        return Err("name cannot be empty".into());
    }
    if s.len() > 64 {
        return Err(format!("name is too long ({} chars, max 64)", s.len()));
    }
    if s.split('/').any(|seg| {
        seg.is_empty()
            || !seg
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '.' | '_' | '-'))
    }) {
        return Err("each path segment may only contain letters, digits, '.', '_', '-'".into());
    }
    Ok(())
}

#[async_trait]
impl Tool for EnterWorktreeTool {
    fn name(&self) -> &str {
        "EnterWorktree"
    }

    fn description(&self) -> &str {
        "Creates an isolated git worktree and switches the session into it. \
         Use this to make changes in isolation without affecting the current branch. \
         Call ExitWorktree when done to return to the original working directory."
    }

    fn input_schema(&self) -> ToolInputSchema {
        serde_json::from_value(json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Optional name for the worktree. Each '/'-separated segment \
                                    may contain only letters, digits, dots, underscores, and dashes; \
                                    max 64 chars total. A random name is generated if not provided."
                }
            }
        }))
        .unwrap()
    }

    async fn execute(&self, input: Value, cancel: &CancellationToken) -> CcResult<ToolResult> {
        let name_opt = input.get("name").and_then(Value::as_str);

        // Validate name if provided
        if let Some(name) = name_opt {
            if let Err(e) = validate_slug(name) {
                return Ok(ToolResult::error(format!("invalid worktree name: {e}")));
            }
        }

        let slug = name_opt
            .map(str::to_string)
            .unwrap_or_else(|| format!("wt-{}", &uuid_slug()));

        let branch = format!("worktree/{slug}");

        // Find git root
        let cwd = std::env::current_dir().unwrap_or_default();
        let git_root_output = tokio::process::Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .current_dir(&cwd)
            .output()
            .await;

        let git_root = match git_root_output {
            Ok(o) if o.status.success() => {
                std::path::PathBuf::from(String::from_utf8_lossy(&o.stdout).trim().to_string())
            }
            _ => return Ok(ToolResult::error("not in a git repository")),
        };

        // Create worktree dir
        let worktrees_dir = git_root.join(".git").join("cc-worktrees");
        let worktree_path = worktrees_dir.join(&slug);

        // Check cancellation
        if cancel.is_cancelled() {
            return Ok(ToolResult::error("cancelled"));
        }

        // Run git worktree add
        let output = tokio::process::Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                &branch,
                worktree_path.to_str().unwrap_or("."),
            ])
            .current_dir(&git_root)
            .output()
            .await;

        match output {
            Ok(o) if o.status.success() => {
                let path_str = worktree_path.to_string_lossy().to_string();
                let result = json!({
                    "worktreePath": path_str,
                    "worktreeBranch": branch,
                    "message": format!(
                        "Created worktree at {path_str} on branch '{branch}'. \
                         You are now working in the worktree. \
                         Call ExitWorktree when done."
                    )
                });
                Ok(ToolResult::ok(result.to_string()))
            }
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                Ok(ToolResult::error(format!(
                    "git worktree add failed: {stderr}"
                )))
            }
            Err(e) => Ok(ToolResult::error(format!("failed to run git: {e}"))),
        }
    }
}

/// Generate a short random hex slug (8 chars).
fn uuid_slug() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    format!("{t:08x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_slug() {
        assert!(validate_slug("my-feature").is_ok());
        assert!(validate_slug("feat/sub-task").is_ok());
        assert!(validate_slug("wt_123").is_ok());
    }

    #[test]
    fn invalid_slug_empty() {
        assert!(validate_slug("").is_err());
    }

    #[test]
    fn invalid_slug_too_long() {
        assert!(validate_slug(&"a".repeat(65)).is_err());
    }

    #[test]
    fn invalid_slug_bad_chars() {
        assert!(validate_slug("feat/sub task").is_err()); // space
        assert!(validate_slug("feat@1").is_err()); // @
    }
}
