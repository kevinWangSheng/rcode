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

        let paths: Vec<String> = matches.into_iter().map(|(_, p)| p).collect();
        Ok(ToolResult::ok(paths.join("\n")))
    }
}
