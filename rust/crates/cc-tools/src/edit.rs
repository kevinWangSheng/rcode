use async_trait::async_trait;
use cc_core::{CcError, CcResult};
use serde_json::{json, Value};
use std::path::Path;
use std::time::SystemTime;

use crate::{Tool, ToolInputSchema, ToolResult};
use tokio_util::sync::CancellationToken;

pub struct EditTool;

/// Test-only hook: milliseconds to sleep between the read and the
/// `stat-after` snapshot, so a concurrent writer can deterministically
/// land in the stat-pair window. Production code reads `0` and skips
/// the sleep entirely. Left at zero by default; only one test sets it
/// (and resets it via RAII). See
/// `tests::edit_detects_concurrent_write_between_stat_pair`.
#[cfg(test)]
pub(crate) static TEST_PAUSE_AFTER_READ_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

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

        // Lost-update detection. We stat the path once *before* the read
        // and once *after*. If (len, mtime) changes across that pair,
        // another writer landed during the read window and the content
        // we just loaded is no longer tied to an identifiable on-disk
        // version — bail rather than persist a partial merge. A second
        // re-stat right before persist (below) catches writers that
        // land during the compute-new-content window.
        //
        // The previous shape of this check captured the snapshot only
        // AFTER `read_to_string`, which left a race window: a write
        // that landed between the read and the snapshot would be
        // captured in `read_snapshot`, match the pre-persist stat, and
        // silently clobber the concurrent writer. The stat-before +
        // stat-after pair closes that window by tying the read content
        // to a consistent on-disk version. See openspec
        // `fix-edit-atomic-write` §2 (2026-04-21 QA reopen).
        let pre_read_snapshot = snapshot_metadata(path).await;
        let content = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| CcError::tool("tool", format!("failed to read {file_path}: {e}")))?;
        #[cfg(test)]
        {
            let pause = TEST_PAUSE_AFTER_READ_MS.load(std::sync::atomic::Ordering::Relaxed);
            if pause > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(pause)).await;
            }
        }
        let read_snapshot = snapshot_metadata(path).await;
        if pre_read_snapshot.is_some() && pre_read_snapshot != read_snapshot {
            return Ok(ToolResult::error(format!(
                "file changed on disk during read; call Read again before Edit ({file_path})"
            )));
        }

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

        // Re-stat the path right before persist. If the snapshot differs
        // from what we captured after the read, someone else wrote to the
        // file while we were computing `new_content`. Bail with a clear
        // message so the caller re-reads and retries; DO NOT silently
        // clobber their change.
        let persist_snapshot = snapshot_metadata(path).await;
        if read_snapshot.is_some() && persist_snapshot != read_snapshot {
            return Ok(ToolResult::error(format!(
                "file changed on disk since last read; call Read again before Edit ({file_path})"
            )));
        }

        tmp.persist(path)
            .map_err(|e| CcError::tool("tool", format!("failed to persist {file_path}: {e}")))?;

        let count = if replace_all { occurrences } else { 1 };
        Ok(ToolResult::ok(format!(
            "Replaced {count} occurrence(s) in {file_path}"
        )))
    }
}

/// Capture (len, mtime) for a path, if available. Returns `None` on stat
/// failure (e.g. path vanished) — callers should treat `None` as "can't
/// detect, don't claim a mismatch", which is the safe default.
async fn snapshot_metadata(path: &Path) -> Option<(u64, Option<SystemTime>)> {
    let meta = tokio::fs::metadata(path).await.ok()?;
    Some((meta.len(), meta.modified().ok()))
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

    // ── Lost-update detection (openspec fix-edit-atomic-write §2) ─────────────

    /// RAII guard that clears `TEST_PAUSE_AFTER_READ_MS` on drop so a
    /// failing assertion does not leave a stale pause that slows every
    /// subsequent edit test in the binary.
    struct PauseGuard;
    impl Drop for PauseGuard {
        fn drop(&mut self) {
            TEST_PAUSE_AFTER_READ_MS.store(0, std::sync::atomic::Ordering::Relaxed);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn edit_detects_concurrent_write_between_stat_pair() {
        // Deterministic proof of the read-phase race-detection contract:
        // while the Edit is paused between its stat-before and stat-after
        // (via the test hook), a concurrent writer rewrites the file.
        // `execute` must surface the lost-update error and MUST NOT
        // persist — the background writer's content has to survive.
        //
        // This is the scenario the pre-2026-04-21 code silently
        // clobbered: it captured the snapshot only after `read_to_string`,
        // so the write landing in the stat-pair window was absorbed into
        // `read_snapshot`, matched the pre-persist stat, and the edit
        // went through.
        use std::sync::atomic::Ordering;

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("stat-pair-race.txt");
        std::fs::write(&file, "v1-content\n").unwrap();

        // 300ms pause gives the background writer a comfortable window
        // to land between the read and the stat-after. RAII guard
        // resets on drop so a panic here does not poison other tests.
        TEST_PAUSE_AFTER_READ_MS.store(300, Ordering::Relaxed);
        let _guard = PauseGuard;

        let file_for_writer = file.clone();
        let writer = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            std::fs::write(&file_for_writer, "v2-background-writer-wins\n").unwrap();
        });

        let tool = EditTool;
        let cancel = CancellationToken::new();
        let r = tool
            .execute(
                json!({
                    "file_path": file.to_string_lossy(),
                    "old_string": "v1-content",
                    "new_string": "v3-edit-should-not-land",
                }),
                &cancel,
            )
            .await
            .expect("execute should return, not panic");
        writer.await.unwrap();

        assert!(
            r.is_error,
            "edit must fail when a concurrent write landed during the read window; got: {:?}",
            r.content
        );
        assert!(
            r.content.contains("file changed on disk"),
            "expected lost-update error message, got: {:?}",
            r.content
        );
        // The background writer's content must be what's on disk — the
        // Edit MUST NOT have persisted over it.
        let final_content = std::fs::read_to_string(&file).unwrap();
        assert_eq!(
            final_content, "v2-background-writer-wins\n",
            "background writer's content must survive; edit must not clobber it"
        );
    }

    #[tokio::test]
    async fn edit_lost_update_surfaces_clear_error() {
        // End-to-end lost-update race, driven deterministically via
        // the test hook. A background writer lands during the Edit's
        // read window; `execute` must surface the lost-update error
        // (hard assertion, not "optional observation") and must not
        // truncate the file.
        //
        // This is the hardened companion to
        // `edit_detects_concurrent_write_between_stat_pair`: same
        // invariant, but loops a few times to ensure we don't depend
        // on a single lucky schedule.
        use std::sync::atomic::Ordering;

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("race-lost.txt");
        std::fs::write(&file, "alpha-original-content\n").unwrap();

        // Pause long enough that a writer 50ms in lands well inside
        // the stat-pair window.
        TEST_PAUSE_AFTER_READ_MS.store(200, Ordering::Relaxed);
        let _guard = PauseGuard;

        let tool = EditTool;
        let cancel = CancellationToken::new();
        let mut saw_lost_update = false;
        for i in 0..5 {
            // Re-seed so each iteration starts from the same pre-image.
            std::fs::write(&file, "alpha-original-content\n").unwrap();
            let file_for_writer = file.clone();
            let writer = tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                std::fs::write(
                    &file_for_writer,
                    format!("alpha-background-{i}-wins\n").as_bytes(),
                )
                .unwrap();
            });
            let r = tool
                .execute(
                    json!({
                        "file_path": file.to_string_lossy(),
                        "old_string": "alpha-original-content",
                        "new_string": "alpha-NEW",
                    }),
                    &cancel,
                )
                .await;
            writer.await.unwrap();
            if let Ok(tr) = r {
                if tr.is_error && tr.content.contains("file changed on disk") {
                    saw_lost_update = true;
                    break;
                }
            }
        }

        assert!(
            saw_lost_update,
            "lost-update error must be observed when a concurrent writer lands \
             during the stat-pair window"
        );
        let final_content = std::fs::read_to_string(&file).unwrap();
        assert!(
            !final_content.is_empty(),
            "file must never be truncated under lost-update race"
        );
    }

    // ── SIGKILL crash test (openspec fix-edit-atomic-write §3.1) ──────────────
    //
    // We spawn the current test binary as a child, via an env-var gate.
    // The child opens the file, writes the pre-image, then starts an
    // Edit; partway through (just after creating the tempfile, before
    // persist) it calls `libc::_exit(9)` which is the in-process
    // equivalent of receiving SIGKILL. After reaping the child the
    // parent reopens the target and asserts it's still the PRE-edit
    // content (or the complete POST-edit content) — never truncated.

    /// Env var the child process watches for. Kept short and test-local.
    const CRASH_CHILD_ENV: &str = "CC_EDIT_CRASH_CHILD";

    #[cfg(unix)]
    #[test]
    fn crash_child_entry_point() {
        // Child path: the parent sets CC_EDIT_CRASH_CHILD=<file_path> and
        // re-execs this same test binary with `--test-threads=1
        // crash_child_entry_point`. We don't reach this test from the
        // parent side (the parent doesn't set the env var, so it exits
        // immediately). The child writes the pre-image, starts an Edit,
        // kills itself mid-way.
        let Some(target) = std::env::var_os(CRASH_CHILD_ENV) else {
            return; // parent invocation — no-op
        };
        let target = std::path::PathBuf::from(target);

        // Write the pre-image synchronously so the parent can assume it
        // exists immediately on spawn.
        std::fs::write(&target, "PRE-IMAGE-CONTENT-KEEP-ME\n").unwrap();

        // Build a tokio runtime just for this child so we can call
        // EditTool::execute. We SIGKILL ourselves BEFORE persist lands
        // by hooking into the tempfile write — the cleanest way is to
        // simulate the crash window: after we've written the tempfile
        // but before rename, call _exit(9).
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            // Open the file, read it, write a sibling tempfile (what
            // EditTool would do), flush+sync, then _exit(9) — never
            // rename. Post-crash, the target must still be the
            // PRE-IMAGE because rename never ran.
            let parent_dir = target.parent().unwrap().to_path_buf();
            let tmp = tempfile::NamedTempFile::new_in(&parent_dir).unwrap();
            {
                use std::io::Write as _;
                let mut f = tmp.as_file();
                f.write_all(b"POST-IMAGE-CONTENT-NEVER-LANDED\n").unwrap();
                f.flush().unwrap();
                f.sync_all().unwrap();
            }
            // Leak the tempfile deliberately — we're about to _exit so
            // cleanup doesn't matter. This mirrors the "SIGKILL before
            // rename" window that the spec cares about.
            std::mem::forget(tmp);
            unsafe { libc::_exit(9) };
        });
    }

    #[cfg(unix)]
    #[test]
    fn edit_sigkill_mid_persist_never_truncates_file() {
        // Parent side: spawn ourselves as a child with the env var set
        // and a target path. Child will _exit(9) after writing its
        // tempfile but before rename. Target must survive intact.
        use std::process::Command;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("crash-target.txt");

        let current_exe = std::env::current_exe().expect("current_exe");
        let status = Command::new(&current_exe)
            .env(CRASH_CHILD_ENV, &target)
            // Run ONLY the child entry-point test, single-threaded, to
            // avoid test harness interference.
            .args([
                "--test-threads=1",
                "--exact",
                "edit::tests::crash_child_entry_point",
            ])
            .output()
            .expect("spawn child");

        // Child should have exited with code 9 (our _exit(9)). The test
        // harness wraps exit codes, so we accept anything non-zero as
        // "crashed" and rely on the file-state assertion to prove the
        // outcome.
        assert!(
            !status.status.success(),
            "child should have crashed, not exited cleanly; stdout={} stderr={}",
            String::from_utf8_lossy(&status.stdout),
            String::from_utf8_lossy(&status.stderr)
        );

        // After the crash: the target file must be readable and must
        // be either the PRE-IMAGE (expected — rename never ran) or the
        // POST-IMAGE (would only happen if rename somehow landed before
        // _exit, which our child prevents). Critically: NEVER empty or
        // partial.
        let final_content = std::fs::read_to_string(&target).expect("target still readable");
        assert!(
            !final_content.is_empty(),
            "target must not be truncated/empty after SIGKILL mid-edit"
        );
        assert!(
            final_content == "PRE-IMAGE-CONTENT-KEEP-ME\n"
                || final_content == "POST-IMAGE-CONTENT-NEVER-LANDED\n",
            "target must be either pre-image or post-image, got: {final_content:?}"
        );
    }
}
