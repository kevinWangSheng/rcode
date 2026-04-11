use async_trait::async_trait;
use cc_core::{CcError, CcResult};
use serde_json::{json, Value};
use std::path::Path;

use crate::{Tool, ToolResult, ToolInputSchema};
use tokio_util::sync::CancellationToken;

const MAX_LINES_DEFAULT: usize = 2000;

pub struct ReadTool;

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        "Read"
    }

    fn description(&self) -> &str {
        "Read a file from the local filesystem. \
         Returns file contents with line numbers (cat -n format). \
         Use offset/limit to read specific portions of large files."
    }

    fn input_schema(&self) -> ToolInputSchema {
        serde_json::from_value(json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Absolute path to the file to read"
                },
                "offset": {
                    "type": "number",
                    "description": "Line number to start reading from (1-indexed)"
                },
                "limit": {
                    "type": "number",
                    "description": "Maximum number of lines to read"
                }
            },
            "required": ["file_path"]
        })).unwrap()
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, _cancel: &CancellationToken) -> CcResult<ToolResult> {
        let file_path = input["file_path"]
            .as_str()
            .ok_or_else(|| CcError::tool("tool", "missing 'file_path' field"))?;

        let path = Path::new(file_path);
        if !path.exists() {
            return Ok(ToolResult::error(format!("File not found: {file_path}")));
        }
        if path.is_dir() {
            return Ok(ToolResult::error(format!("{file_path} is a directory")));
        }

        let content = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| CcError::tool("tool", format!("failed to read {file_path}: {e}")))?;

        let offset = input["offset"].as_u64().map(|n| n as usize).unwrap_or(1);
        let limit = input["limit"]
            .as_u64()
            .map(|n| n as usize)
            .unwrap_or(MAX_LINES_DEFAULT);

        let lines: Vec<&str> = content.lines().collect();
        let start = offset.saturating_sub(1); // convert 1-indexed to 0-indexed
        let end = (start + limit).min(lines.len());

        let numbered: Vec<String> = lines[start..end]
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{}\t{}", start + i + 1, line))
            .collect();

        Ok(ToolResult::ok(numbered.join("\n")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn read_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "line1\nline2\nline3\n").unwrap();

        let tool = ReadTool;
        let cancel = CancellationToken::new();
        let result = tool.execute(
            json!({"file_path": file.to_string_lossy()}),
            &cancel,
        ).await.unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("1\tline1"));
        assert!(result.content.contains("2\tline2"));
        assert!(result.content.contains("3\tline3"));
    }

    #[tokio::test]
    async fn read_not_found() {
        let tool = ReadTool;
        let cancel = CancellationToken::new();
        let result = tool.execute(
            json!({"file_path": "/nonexistent/file.txt"}),
            &cancel,
        ).await.unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("not found"));
    }

    #[tokio::test]
    async fn read_with_offset_and_limit() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lines.txt");
        std::fs::write(&file, "a\nb\nc\nd\ne\n").unwrap();

        let tool = ReadTool;
        let cancel = CancellationToken::new();
        let result = tool.execute(
            json!({"file_path": file.to_string_lossy(), "offset": 2, "limit": 2}),
            &cancel,
        ).await.unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("2\tb"));
        assert!(result.content.contains("3\tc"));
        assert!(!result.content.contains("1\ta"));
    }

    #[test]
    fn read_is_read_only() {
        assert!(ReadTool.is_read_only());
    }
}
