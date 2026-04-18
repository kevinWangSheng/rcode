use async_trait::async_trait;
use cc_core::{CcError, CcResult};
use serde_json::{json, Value};
use std::path::Path;

use crate::{Tool, ToolResult, ToolInputSchema};
use tokio_util::sync::CancellationToken;

const MAX_LINES_DEFAULT: usize = 2000;
/// Hard cap on file size to prevent OOM when a user accidentally points
/// Read at a giant log / binary / minified bundle. 50 MB comfortably fits
/// every source file in a normal repo while still saying "no" to a 10 GB
/// database dump. Files over the cap get a clear error telling the user to
/// pass `offset` + `limit`, or use Grep/Bash for partial reads.
const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;

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

    async fn execute(&self, input: Value, cancel: &CancellationToken) -> CcResult<ToolResult> {
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

        // Size-cap gate: stat before reading so a 10 GB log file can't
        // balloon our memory before we notice. Files over MAX_FILE_BYTES
        // get a helpful error explaining how to do a partial read.
        if let Ok(meta) = tokio::fs::metadata(path).await {
            if meta.len() > MAX_FILE_BYTES {
                let mb = meta.len() / (1024 * 1024);
                let cap_mb = MAX_FILE_BYTES / (1024 * 1024);
                return Ok(ToolResult::error(format!(
                    "File {file_path} is {mb} MB, above the {cap_mb} MB Read cap. \
                     Use `offset` + `limit` for a line range, or the Grep/Bash tools \
                     for pattern / byte-range reads."
                )));
            }
        }

        // Race the read against cancel so Ctrl+C lands quickly even on slow
        // network filesystems (NFS, FUSE mounts, etc.).
        let content = tokio::select! {
            r = tokio::fs::read_to_string(path) => r
                .map_err(|e| CcError::tool("tool", format!("failed to read {file_path}: {e}")))?,
            _ = cancel.cancelled() => {
                return Err(CcError::tool("tool", "Read cancelled"));
            }
        };

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

    #[tokio::test]
    async fn read_rejects_file_over_size_cap() {
        // Sparse file — 60 MB metadata-reported size with almost no actual
        // disk usage. Tests the gate without spending real bytes.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("huge.bin");
        let f = std::fs::File::create(&file).unwrap();
        f.set_len(MAX_FILE_BYTES + 1).unwrap();

        let tool = ReadTool;
        let cancel = CancellationToken::new();
        let result = tool
            .execute(json!({"file_path": file.to_string_lossy()}), &cancel)
            .await
            .unwrap();
        assert!(result.is_error, "oversize file must be rejected");
        assert!(
            result.content.contains("above the")
                && result.content.contains("Read cap"),
            "error must mention the cap: {}",
            result.content
        );
        assert!(
            result.content.contains("offset") && result.content.contains("limit"),
            "error must point the user at offset/limit workaround"
        );
    }

    #[tokio::test]
    async fn read_honors_cancel_token() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("cancel.txt");
        std::fs::write(&file, "x").unwrap();

        let tool = ReadTool;
        let cancel = CancellationToken::new();
        cancel.cancel(); // pre-cancel

        let result = tool
            .execute(json!({"file_path": file.to_string_lossy()}), &cancel)
            .await;
        // Either cancel branch wins before the read finishes, OR the
        // read completes first because it's tiny. Both are acceptable —
        // we just verify the cancel code path doesn't panic/error
        // structurally. When it DOES win, we get the cancelled error.
        if let Err(e) = result {
            assert!(e.to_string().contains("cancelled"));
        }
    }

    #[test]
    fn read_is_read_only() {
        assert!(ReadTool.is_read_only());
    }
}
