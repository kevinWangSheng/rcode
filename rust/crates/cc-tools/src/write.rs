use async_trait::async_trait;
use cc_core::{CcError, CcResult};
use serde_json::{json, Value};
use std::path::Path;

use crate::{Tool, ToolContext, ToolInputSchema, ToolResult};

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
        }))
        .unwrap()
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> CcResult<ToolResult> {
        let file_path = input["file_path"]
            .as_str()
            .ok_or_else(|| CcError::tool("tool", "missing 'file_path' field"))?;
        let content = input["content"]
            .as_str()
            .ok_or_else(|| CcError::tool("tool", "missing 'content' field"))?;

        let path = Path::new(file_path);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                CcError::tool(
                    "tool",
                    format!("failed to create dirs for {file_path}: {e}"),
                )
            })?;
        }

        // Capture the existing file mode (if any) so we can preserve it
        // across the write. `tokio::fs::write` with a path only guarantees
        // umask-driven perms on create, and whether it preserves mode on
        // overwrite is OS/filesystem dependent. A `chmod +x` script must
        // stay executable after an LLM-driven Write.
        let preserved_mode = match tokio::fs::metadata(path).await {
            Ok(meta) => Some(meta.permissions()),
            Err(_) => None,
        };

        // Write atomically via same-directory tempfile + rename so a mid-
        // write crash doesn't leave the file truncated. Preserve the mode on
        // the tempfile before persist.
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let tmp = tempfile::NamedTempFile::new_in(parent).map_err(|e| {
            CcError::tool(
                "tool",
                format!("failed to create tempfile near {file_path}: {e}"),
            )
        })?;

        // Apply preserved mode before writing contents. On Unix the mode
        // bits (esp. the executable bit) are what we care about.
        if let Some(perms) = &preserved_mode {
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
                let _ = perms; // keep the binding used on non-unix
                let file = tmp.as_file();
                let _ = file.set_permissions(perms.clone());
            }
        }

        // Write + fsync the tempfile before the atomic rename.
        {
            use std::io::Write as _;
            let mut file = tmp.as_file();
            file.write_all(content.as_bytes())
                .map_err(|e| CcError::tool("tool", format!("failed to write {file_path}: {e}")))?;
            file.flush()
                .map_err(|e| CcError::tool("tool", format!("failed to flush {file_path}: {e}")))?;
            file.sync_all()
                .map_err(|e| CcError::tool("tool", format!("failed to fsync {file_path}: {e}")))?;
        }

        tmp.persist(path)
            .map_err(|e| CcError::tool("tool", format!("failed to persist {file_path}: {e}")))?;

        Ok(ToolResult::ok(format!(
            "File written successfully to {file_path}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn write_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("new.txt");

        let tool = WriteTool;
        let ctx = ToolContext::for_test_bare(CancellationToken::new());
        let result = tool
            .execute(
                json!({"file_path": file.to_string_lossy(), "content": "hello world"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello world");
    }

    #[tokio::test]
    async fn write_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a").join("b").join("c.txt");

        let tool = WriteTool;
        let ctx = ToolContext::for_test_bare(CancellationToken::new());
        let result = tool
            .execute(
                json!({"file_path": file.to_string_lossy(), "content": "deep"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "deep");
    }

    #[tokio::test]
    async fn write_overwrites_existing() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("existing.txt");
        std::fs::write(&file, "old content").unwrap();

        let tool = WriteTool;
        let ctx = ToolContext::for_test_bare(CancellationToken::new());
        let result = tool
            .execute(
                json!({"file_path": file.to_string_lossy(), "content": "new content"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new content");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn write_preserves_existing_mode() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("script.sh");
        std::fs::write(&file, "#!/bin/sh\necho hi\n").unwrap();
        // chmod +x
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();

        let tool = WriteTool;
        let ctx = ToolContext::for_test_bare(CancellationToken::new());
        let result = tool
            .execute(
                json!({"file_path": file.to_string_lossy(), "content": "#!/bin/sh\necho new\n"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error);

        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "executable bit must be preserved across Write");
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "#!/bin/sh\necho new\n"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn write_new_file_uses_default_umask() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("fresh.txt");

        let tool = WriteTool;
        let ctx = ToolContext::for_test_bare(CancellationToken::new());
        let result = tool
            .execute(
                json!({"file_path": file.to_string_lossy(), "content": "hello"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error);

        // New file: we don't set mode, tempfile + rename yields umask-derived
        // perms. 0o600 is the tempfile default; anything without the
        // executable bit is acceptable — assert the critical property: NOT
        // executable.
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode & 0o111,
            0,
            "new file must not acquire exec bits: {mode:o}"
        );
    }
}
