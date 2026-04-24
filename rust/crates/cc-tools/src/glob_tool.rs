use async_trait::async_trait;
use cc_core::{CcError, CcResult};
use serde_json::{json, Value};
use std::path::Path;

use crate::{Tool, ToolContext, ToolInputSchema, ToolResult};

pub struct GlobTool;

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "Glob"
    }

    fn description(&self) -> &str {
        "Fast file pattern matching. Supports glob patterns like '**/*.rs' or 'src/**/*.ts'. \
         Returns matching file paths sorted by modification time."
    }

    fn input_schema(&self) -> ToolInputSchema {
        serde_json::from_value(json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern to match files against"
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search in (defaults to current directory)"
                }
            },
            "required": ["pattern"]
        }))
        .unwrap()
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> CcResult<ToolResult> {
        let pattern = input["pattern"]
            .as_str()
            .ok_or_else(|| CcError::tool("tool", "missing 'pattern' field"))?;

        let base_dir = input["path"]
            .as_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                std::env::current_dir()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string()
            });

        // Build full pattern relative to base_dir
        let full_pattern = if Path::new(pattern).is_absolute() {
            pattern.to_string()
        } else {
            format!("{}/{}", base_dir.trim_end_matches('/'), pattern)
        };

        let mut matches: Vec<(std::time::SystemTime, String)> = Vec::new();

        for entry in glob::glob(&full_pattern)
            .map_err(|e| CcError::tool("tool", format!("invalid glob pattern: {e}")))?
            .flatten()
        {
            // A huge repo with a broad glob (e.g. `**/*`) can iterate
            // hundreds of thousands of entries before returning. Peek at
            // the cancel token each iteration so Ctrl+C lands quickly.
            if ctx.cancel.is_cancelled() {
                return Err(CcError::tool("tool", "Glob cancelled"));
            }
            if entry.is_file() {
                let mtime = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH);
                let path_str = entry.to_string_lossy().to_string();
                matches.push((mtime, path_str));
            }
        }

        // Sort by modification time (newest first)
        matches.sort_by(|a, b| b.0.cmp(&a.0));

        if matches.is_empty() {
            return Ok(ToolResult::ok("No files matched the pattern."));
        }

        // Drop git-ignored matches so `**/*.log` doesn't surface
        // committed-by-accident build detritus (P0 #3). `filter_git_ignored`
        // batches to a single `git check-ignore --stdin` call; on timeout
        // or outside a repo it returns all-false and we keep every match.
        let path_bufs: Vec<std::path::PathBuf> = matches
            .iter()
            .map(|(_, p)| std::path::PathBuf::from(p))
            .collect();
        let borrow: Vec<&Path> = path_bufs.iter().map(|p| p.as_path()).collect();
        let cwd = Path::new(&base_dir);
        let ignored_flags = cc_git::filter_git_ignored(&borrow, cwd).await;
        let paths: Vec<String> = matches
            .into_iter()
            .zip(ignored_flags)
            .filter_map(|((_, p), ignored)| (!ignored).then_some(p))
            .collect();

        if paths.is_empty() {
            return Ok(ToolResult::ok(
                "No files matched the pattern (all matches were git-ignored).",
            ));
        }

        Ok(ToolResult::ok(paths.join("\n")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    /// P0 #3: Glob must drop git-ignored matches.
    #[tokio::test]
    async fn glob_drops_gitignored_matches() {
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("skipping: git binary not available");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        let run = |args: &[&str]| {
            assert!(std::process::Command::new("git")
                .args(args)
                .current_dir(repo)
                .status()
                .unwrap()
                .success());
        };
        run(&["init", "-q"]);
        std::fs::write(repo.join(".gitignore"), "*.log\n").unwrap();
        std::fs::write(repo.join("keep.rs"), "fn main(){}").unwrap();
        std::fs::write(repo.join("drop.log"), "noise").unwrap();

        let tool = GlobTool;
        let ctx = ToolContext::for_test_bare(CancellationToken::new());
        let result = tool
            .execute(
                json!({"pattern": "*", "path": repo.to_string_lossy()}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(result.content.contains("keep.rs"), "{}", result.content);
        assert!(!result.content.contains("drop.log"), "{}", result.content);
    }
}
