use async_trait::async_trait;
use cc_core::{CcError, CcResult};
use serde_json::{json, Value};
use std::path::Path;

use crate::{Tool, ToolInputSchema, ToolResult};
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
        }))
        .unwrap()
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

        // Capture the existing mode so we can restore it on the tempfile
        // before the rename. Without this a `chmod +x` script edited via the
        // Edit tool loses its executable bit and stops working.
        let preserved_perms = tokio::fs::metadata(path)
            .await
            .ok()
            .map(|m| m.permissions());

        // Atomic write: same-directory tempfile + fsync + rename. Three bugs
        // this closes:
        //   * Truncation window: `tokio::fs::write` opens with truncate+create,
        //     so a SIGKILL between the open and the write leaves an empty
        //     file.
        //   * Lost-update sibling: another writer racing on the same path
        //     would interleave with the truncated write and produce a
        //     partial merge.
        //   * Durability: without fsync + rename, a power loss right after
        //     "success" can still roll the file back.
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let tmp = tempfile::NamedTempFile::new_in(parent).map_err(|e| {
            CcError::tool(
                "tool",
                format!("failed to create tempfile near {file_path}: {e}"),
            )
        })?;

        // Preserve mode before persist so the rename lands with the right
        // perms. On Unix we care primarily about the exec bit.
        if let Some(perms) = &preserved_perms {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = perms.mode();
                let file = tmp.as_file();
                let mut tmp_perms = file
                    .metadata()
                    .map_err(|e| CcError::tool("tool", format!("failed to stat tempfile: {e}")))?
                    .permissions();
                tmp_perms.set_mode(mode);
                file.set_permissions(tmp_perms)
                    .map_err(|e| CcError::tool("tool", format!("failed to chmod tempfile: {e}")))?;
            }
            #[cfg(not(unix))]
            {
                let file = tmp.as_file();
                let _ = file.set_permissions(perms.clone());
            }
        }

        {
            use std::io::Write as _;
            let mut file = tmp.as_file();
            file.write_all(new_content.as_bytes())
                .map_err(|e| CcError::tool("tool", format!("failed to write {file_path}: {e}")))?;
            file.flush()
                .map_err(|e| CcError::tool("tool", format!("failed to flush {file_path}: {e}")))?;
            file.sync_all()
                .map_err(|e| CcError::tool("tool", format!("failed to fsync {file_path}: {e}")))?;
        }

        tmp.persist(path)
            .map_err(|e| CcError::tool("tool", format!("failed to persist {file_path}: {e}")))?;

        let count = if replace_all { occurrences } else { 1 };
        Ok(ToolResult::ok(format!(
            "Replaced {count} occurrence(s) in {file_path}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn edit_basic_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("t.txt");
        std::fs::write(&file, "hello world").unwrap();

        let tool = EditTool;
        let cancel = CancellationToken::new();
        let r = tool
            .execute(
                json!({"file_path": file.to_string_lossy(), "old_string": "world", "new_string": "rust"}),
                &cancel,
            )
            .await
            .unwrap();
        assert!(!r.is_error, "{}", r.content);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello rust");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn edit_preserves_mode() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("s.sh");
        std::fs::write(&file, "#!/bin/sh\necho a\n").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();

        let tool = EditTool;
        let cancel = CancellationToken::new();
        let r = tool
            .execute(
                json!({"file_path": file.to_string_lossy(), "old_string": "echo a", "new_string": "echo b"}),
                &cancel,
            )
            .await
            .unwrap();
        assert!(!r.is_error);

        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "exec bit must be preserved across Edit");
    }

    #[tokio::test]
    async fn edit_concurrent_writers_never_truncate() {
        // Two concurrent Edit calls on the same file. The rename is atomic so
        // one wins, but nothing should ever be *truncated* (empty / partial).
        // The file must be a complete previous version or a complete new
        // version after each call.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("race.txt");
        let initial = "aaa\nbbb\nccc\n";
        std::fs::write(&file, initial).unwrap();

        let file_a = file.clone();
        let file_b = file.clone();
        let tool_a = EditTool;
        let tool_b = EditTool;
        let cancel = CancellationToken::new();
        let cancel2 = cancel.clone();

        let (ra, rb) = tokio::join!(
            tool_a.execute(
                json!({"file_path": file_a.to_string_lossy(), "old_string": "aaa", "new_string": "AAA"}),
                &cancel,
            ),
            tool_b.execute(
                json!({"file_path": file_b.to_string_lossy(), "old_string": "bbb", "new_string": "BBB"}),
                &cancel2,
            ),
        );
        // Both execute through to their own tempfile + persist. Success is
        // not required from both — one may surface a "not found" if the
        // other's rename landed first. What IS required: the file is never
        // truncated to empty.
        let _ = ra;
        let _ = rb;

        let final_content = std::fs::read_to_string(&file).unwrap();
        assert!(!final_content.is_empty(), "file must not be truncated");
        // Must contain at least the bbb/ccc lines (one edit landed) or
        // originals.
        assert!(final_content.contains("ccc"), "trailing content preserved");
    }

    #[tokio::test]
    async fn edit_missing_file_returns_error() {
        let tool = EditTool;
        let cancel = CancellationToken::new();
        let r = tool
            .execute(
                json!({"file_path": "/nonexistent/xyz.txt", "old_string": "a", "new_string": "b"}),
                &cancel,
            )
            .await
            .unwrap();
        assert!(r.is_error);
        assert!(r.content.contains("not found"));
    }
}
