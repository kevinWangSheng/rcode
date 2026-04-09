use async_trait::async_trait;
use cc_core::{CcError, CcResult};
use serde_json::{json, Value};
use std::path::Path;

use crate::{Tool, ToolResult, ToolInputSchema};
use tokio_util::sync::CancellationToken;

pub struct WriteTool;

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "Write"
    }

    fn description(&self) -> &str {
        "Write a file to the local filesystem. \
         Creates parent directories as needed. \
         Overwrites the file if it already exists."
    }

    fn input_schema(&self) -> ToolInputSchema {
        serde_json::from_value(json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Absolute path to the file to write"
                },
                "content": {
                    "type": "string",
                    "description": "The content to write to the file"
                }
            },
            "required": ["file_path", "content"]
        })).unwrap()
    }

    async fn execute(&self, input: Value, _cancel: &CancellationToken) -> CcResult<ToolResult> {
        let file_path = input["file_path"]
            .as_str()
            .ok_or_else(|| CcError::tool("tool", "missing 'file_path' field"))?;
        let content = input["content"]
            .as_str()
            .ok_or_else(|| CcError::tool("tool", "missing 'content' field"))?;

        let path = Path::new(file_path);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| CcError::tool("tool", format!("failed to create dirs for {file_path}: {e}")))?;
        }

        tokio::fs::write(path, content)
            .await
            .map_err(|e| CcError::tool("tool", format!("failed to write {file_path}: {e}")))?;

        Ok(ToolResult::ok(format!("File written successfully to {file_path}")))
    }
}
