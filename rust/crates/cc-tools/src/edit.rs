use async_trait::async_trait;
use cc_core::{CcError, CcResult};
use serde_json::{json, Value};
use std::path::Path;

use crate::{Tool, ToolResult, ToolInputSchema};
use tokio_util::sync::CancellationToken;

pub struct EditTool;

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "Edit"
    }

    fn description(&self) -> &str {
        "Perform exact string replacements in files. \
         Fails if old_string is not found or is not unique (unless replace_all=true). \
         Requires reading the file first to ensure correct indentation."
    }

    fn input_schema(&self) -> ToolInputSchema {
        serde_json::from_value(json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Absolute path to the file to modify"
                },
                "old_string": {
                    "type": "string",
                    "description": "The exact text to replace"
                },
                "new_string": {
                    "type": "string",
                    "description": "The text to replace it with"
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "Replace all occurrences (default: false)"
                }
            },
            "required": ["file_path", "old_string", "new_string"]
        })).unwrap()
    }

    async fn execute(&self, input: Value, _cancel: &CancellationToken) -> CcResult<ToolResult> {
        let file_path = input["file_path"]
            .as_str()
            .ok_or_else(|| CcError::tool("tool", "missing 'file_path' field"))?;
        let old_string = input["old_string"]
            .as_str()
            .ok_or_else(|| CcError::tool("tool", "missing 'old_string' field"))?;
        let new_string = input["new_string"]
            .as_str()
            .ok_or_else(|| CcError::tool("tool", "missing 'new_string' field"))?;
        let replace_all = input["replace_all"].as_bool().unwrap_or(false);

        let path = Path::new(file_path);
        if !path.exists() {
            return Ok(ToolResult::error(format!("File not found: {file_path}")));
        }

        let content = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| CcError::tool("tool", format!("failed to read {file_path}: {e}")))?;

        let occurrences = content.matches(old_string).count();
        if occurrences == 0 {
            return Ok(ToolResult::error(format!(
                "old_string not found in {file_path}"
            )));
        }
        if occurrences > 1 && !replace_all {
            return Ok(ToolResult::error(format!(
                "old_string is not unique in {file_path} ({occurrences} occurrences). \
                 Provide more context or use replace_all=true."
            )));
        }

        let new_content = if replace_all {
            content.replace(old_string, new_string)
        } else {
            content.replacen(old_string, new_string, 1)
        };

        tokio::fs::write(path, &new_content)
            .await
            .map_err(|e| CcError::tool("tool", format!("failed to write {file_path}: {e}")))?;

        let count = if replace_all { occurrences } else { 1 };
        Ok(ToolResult::ok(format!(
            "Replaced {count} occurrence(s) in {file_path}"
        )))
    }
}
