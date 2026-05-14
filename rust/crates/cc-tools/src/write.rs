use async_trait::async_trait;
use cc_core::{CcError, CcResult};
use serde_json::{json, Value};
use std::path::Path;

use crate::file_history::{project_relative_path, MAX_SNAPSHOT_BYTES};
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

    async fn check_permissions(&self, input: &Value) -> Option<cc_core::PermissionResult> {
        let path = input.get("file_path")?.as_str()?;
        crate::path_safety::safety_check_path(path)
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> CcResult<ToolResult> {
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

        // File-history snapshot: if the target pre-exists, read its
        // current bytes and persist them via the session sink BEFORE the
        // atomic write. Matches TS `fileHistoryTrackEdit` semantics
        // (backup only covers files that already existed) and the
        // orphan-snapshot rule in
        // openspec `fix-file-history-snapshot-producers` §Open Questions.
        let file_existed = path.exists();
        if file_existed {
            let prior = tokio::fs::read(path).await.map_err(|e| {
                CcError::tool(
                    "tool",
                    format!("failed to snapshot pre-write contents of {file_path}: {e}"),
                )
            })?;
            if prior.len() > MAX_SNAPSHOT_BYTES {
                tracing::warn!(
                    path = %file_path,
                    size = prior.len(),
                    threshold = MAX_SNAPSHOT_BYTES,
                    "file-history snapshot exceeds threshold; persisting anyway"
                );
            }
            let relpath = project_relative_path(path);
            let message_id = ctx.message_id.clone().unwrap_or_default();
            ctx.session.append_file_history_snapshot_for_path(
                &relpath,
                &message_id,
                &prior,
                false,
            )?;
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
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    /// Build a `ToolContext` whose session sink is a real
    /// `cc_session::Session` rooted inside `session_dir`. Used by the
    /// file-history regression tests; returns the concrete Session too
    /// so assertions can call `session.file_history_snapshots()` and
    /// `session.read_backup(...)`.
    fn ctx_with_real_session(
        session_dir: &std::path::Path,
        message_id: Option<&str>,
    ) -> (ToolContext, Arc<cc_session::Session>) {
        // Lay out the session as `{dir}/<session-id>/transcript.jsonl`
        // so `session_dir()` has a parent to hang the backup dir off.
        let id = "test-write";
        let sdir = session_dir.join(id);
        std::fs::create_dir_all(&sdir).unwrap();
        let transcript = sdir.join("transcript.jsonl");
        let session = Arc::new(cc_session::Session::from_parts(id.into(), transcript));
        let ctx = ToolContext {
            session: session.clone() as Arc<dyn cc_core::SessionSink>,
            cancel: CancellationToken::new(),
            message_id: message_id.map(str::to_owned),
        };
        (ctx, session)
    }

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

    // ── File-history snapshot producers (openspec
    // `fix-file-history-snapshot-producers` §4) ──────────────────────────

    #[tokio::test]
    async fn write_overwrite_appends_snapshot() {
        // A Write against an existing file MUST persist the pre-write
        // bytes via the session sink, so resume can restore the file to
        // its pre-edit state.
        let work = tempfile::tempdir().unwrap();
        let session_root = tempfile::tempdir().unwrap();
        let file = work.path().join("foo.txt");
        std::fs::write(&file, "old").unwrap();

        let (ctx, session) = ctx_with_real_session(session_root.path(), Some("msg-overwrite"));

        let tool = WriteTool;
        let result = tool
            .execute(
                json!({"file_path": file.to_string_lossy(), "content": "new"}),
                &ctx,
            )
            .await
            .expect("Write must succeed");
        assert!(!result.is_error, "{}", result.content);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new");

        let snaps = session.file_history_snapshots().unwrap();
        assert_eq!(snaps.len(), 1, "exactly one snapshot for one overwrite");
        let snap = &snaps[0];
        assert_eq!(snap.message_id, "msg-overwrite");
        assert!(
            !snap.is_snapshot_update,
            "overwrite snapshot must not be a refinement"
        );
        assert_eq!(
            snap.snapshot.tracked_file_backups.len(),
            1,
            "exactly one tracked backup entry"
        );
        let (relpath, backup) = snap.snapshot.tracked_file_backups.iter().next().unwrap();
        let backup_name = backup
            .backup_file_name
            .clone()
            .expect("overwrite backup must reference a sidecar");
        let bytes = session.read_backup(&backup_name).unwrap();
        assert_eq!(
            bytes, b"old",
            "sidecar must hold the pre-write bytes for {relpath}"
        );
    }

    #[tokio::test]
    async fn two_writes_same_file_persist_distinct_backups() {
        // Two successful Writes overwriting the same file MUST
        // persist two distinct sidecars holding each Write's pre-
        // mutation bytes. Regression guard for
        // fix-file-history-backup-versioning — the `@v1`-fixed
        // producer silently overwrote the first sidecar.
        let work = tempfile::tempdir().unwrap();
        let session_root = tempfile::tempdir().unwrap();
        let file = work.path().join("versions.txt");
        std::fs::write(&file, "a").unwrap();

        let (ctx, session) = ctx_with_real_session(session_root.path(), Some("msg-two-writes"));
        let tool = WriteTool;

        let r1 = tool
            .execute(
                json!({"file_path": file.to_string_lossy(), "content": "b"}),
                &ctx,
            )
            .await
            .expect("first write returns");
        assert!(!r1.is_error, "{}", r1.content);

        let r2 = tool
            .execute(
                json!({"file_path": file.to_string_lossy(), "content": "c"}),
                &ctx,
            )
            .await
            .expect("second write returns");
        assert!(!r2.is_error, "{}", r2.content);

        assert_eq!(std::fs::read_to_string(&file).unwrap(), "c");

        let snaps = session.file_history_snapshots().unwrap();
        assert_eq!(
            snaps.len(),
            2,
            "two successful overwrites must produce two snapshots"
        );
        let name0 = snaps[0]
            .snapshot
            .tracked_file_backups
            .values()
            .next()
            .unwrap()
            .backup_file_name
            .clone()
            .expect("sidecar ref");
        let name1 = snaps[1]
            .snapshot
            .tracked_file_backups
            .values()
            .next()
            .unwrap()
            .backup_file_name
            .clone()
            .expect("sidecar ref");
        assert_ne!(name0, name1, "sidecar names must differ");
        assert!(
            name0.ends_with("@v1") && name1.ends_with("@v2"),
            "expected @v1/@v2, got {name0} and {name1}"
        );
        assert_eq!(
            session.read_backup(&name0).unwrap(),
            b"a",
            "first sidecar must hold pre-write-1 bytes"
        );
        assert_eq!(
            session.read_backup(&name1).unwrap(),
            b"b",
            "second sidecar must hold pre-write-2 bytes (= post-write-1)"
        );
    }

    #[tokio::test]
    async fn write_new_file_does_not_snapshot() {
        // Target path does NOT exist. The Write must succeed but MUST NOT
        // emit a snapshot — matches TS which only snapshots pre-existing
        // files.
        let work = tempfile::tempdir().unwrap();
        let session_root = tempfile::tempdir().unwrap();
        let file = work.path().join("new.txt");

        let (ctx, session) = ctx_with_real_session(session_root.path(), Some("msg-new"));

        let tool = WriteTool;
        let result = tool
            .execute(
                json!({"file_path": file.to_string_lossy(), "content": "hi"}),
                &ctx,
            )
            .await
            .expect("Write must succeed");
        assert!(!result.is_error, "{}", result.content);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hi");

        let snaps = session.file_history_snapshots().unwrap();
        assert!(
            snaps.is_empty(),
            "net-new file must not emit a snapshot, got {snaps:?}"
        );
    }
}
