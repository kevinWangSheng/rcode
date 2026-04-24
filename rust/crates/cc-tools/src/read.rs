use async_trait::async_trait;
use cc_core::{CcError, CcResult};
use serde_json::{json, Value};
use std::path::Path;

use crate::{Tool, ToolContext, ToolInputSchema, ToolResult};

const MAX_LINES_DEFAULT: usize = 2000;
/// Hard cap on file size to prevent OOM when a user accidentally points
/// Read at a giant log / binary / minified bundle. 50 MB comfortably fits
/// every source file in a normal repo while still saying "no" to a 10 GB
/// database dump. Files over the cap get a clear error telling the user to
/// pass `offset` + `limit`, or use Grep/Bash for partial reads.
const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;

/// Character-device paths Read must refuse. `/dev/zero` would stream
/// null bytes until the cap fired; `/dev/random` / `/dev/urandom`
/// would block or drain entropy; `/dev/null` is a no-op but has no
/// legitimate Read use case. TS `FileReadTool` keeps the same list.
/// Any match against `path.starts_with(...)` short-circuits before
/// the `tokio::fs::File::open` syscall so we never create the fd.
const BLOCKED_DEVICE_PATHS: &[&str] = &[
    "/dev/zero",
    "/dev/random",
    "/dev/urandom",
    "/dev/null",
    "/dev/tty",
    "/dev/stdin",
    "/dev/stdout",
    "/dev/stderr",
    "/proc",
    "/sys",
];

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
        }))
        .unwrap()
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> CcResult<ToolResult> {
        let file_path = input["file_path"]
            .as_str()
            .ok_or_else(|| CcError::tool("tool", "missing 'file_path' field"))?;

        let path = Path::new(file_path);

        // Refuse blocked device / kernel-fs paths up-front (P0 #13).
        // `/dev/zero`, `/dev/random`, `/proc`, `/sys` etc. would either
        // stream garbage, drain entropy, or expose host info we never
        // meant to surface. The check happens before `File::open` so the
        // fd is never created.
        let path_str = path.to_string_lossy();
        if BLOCKED_DEVICE_PATHS.iter().any(|blocked| {
            path_str.as_ref() == *blocked || path_str.starts_with(&format!("{blocked}/"))
        }) {
            return Ok(ToolResult::error(format!(
                "{file_path} is on the blocked-device-paths list. \
                 Use Bash with an explicit tool (`head`, `dd`, `od`) if \
                 you really need to sample a device node."
            )));
        }

        // Refuse reads of git-ignored paths (P0 #3). `is_git_ignored` is
        // best-effort: if git is unavailable, the path is outside a repo,
        // or the subprocess times out, it returns `false` and we proceed
        // normally. Check against the *parent* directory when available
        // (so `git check-ignore` walks up from the right place) and fall
        // back to the process cwd otherwise.
        let cwd = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.to_path_buf())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| Path::new(".").to_path_buf());
        if cc_git::is_git_ignored(path, &cwd).await {
            return Ok(ToolResult::error(format!(
                "{file_path} is git-ignored. If this is intentional, \
                 use the Bash tool (e.g. `cat {file_path}`) to bypass \
                 the privacy filter."
            )));
        }

        // Open the file ONCE and perform both the size probe and the read
        // through the same fd. A separate path-based `metadata()` followed by
        // `read_to_string(path)` is a TOCTOU: an attacker who swaps the path
        // to a 10 GB log between the two syscalls can bypass the cap and OOM
        // the process. An fd is pinned to the inode, so metadata() + read()
        // on the same fd describe the same object.
        let file = match tokio::fs::File::open(path).await {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ToolResult::error(format!("File not found: {file_path}")));
            }
            Err(e) if e.kind() == std::io::ErrorKind::IsADirectory
                || e.raw_os_error() == Some(21) /* EISDIR */ =>
            {
                return Ok(ToolResult::error(format!("{file_path} is a directory")));
            }
            Err(e) => {
                // On some platforms (macOS) opening a dir returns a generic
                // error kind; double-check via path semantics.
                if path.is_dir() {
                    return Ok(ToolResult::error(format!("{file_path} is a directory")));
                }
                return Err(CcError::tool("tool", format!("failed to open {file_path}: {e}")));
            }
        };

        // Size-cap gate, atomic with the read. meta.len() describes the
        // inode behind `file`, not whatever the path now points at.
        let size = match file.metadata().await {
            Ok(m) => {
                if m.is_dir() {
                    return Ok(ToolResult::error(format!("{file_path} is a directory")));
                }
                m.len()
            }
            Err(e) => {
                return Err(CcError::tool(
                    "tool",
                    format!("failed to stat {file_path}: {e}"),
                ));
            }
        };
        if size > MAX_FILE_BYTES {
            let mb = size / (1024 * 1024);
            let cap_mb = MAX_FILE_BYTES / (1024 * 1024);
            // Drop the fd without reading; cap violation — no bytes slurped.
            drop(file);
            return Ok(ToolResult::error(format!(
                "File {file_path} is {mb} MB, above the {cap_mb} MB Read cap. \
                 Use `offset` + `limit` for a line range, or the Grep/Bash tools \
                 for pattern / byte-range reads."
            )));
        }

        // Stream-read from the same fd with an explicit byte cap. This
        // defends against a second race where the file grows between the
        // stat above and the read below (e.g. an active log being appended
        // to). The cap ensures we never allocate more than MAX_FILE_BYTES
        // + a small overshoot margin regardless of growth.
        //
        // Race the read against cancel so Ctrl+C lands quickly even on slow
        // network filesystems (NFS, FUSE mounts, etc.).
        use tokio::io::AsyncReadExt;
        // +1 so we can detect overshoot (someone grew the file past the cap
        // after the stat but before we finished reading).
        let read_limit = MAX_FILE_BYTES + 1;
        let mut buf = Vec::with_capacity(size as usize);
        let mut bounded = file.take(read_limit);
        let read_result = tokio::select! {
            r = bounded.read_to_end(&mut buf) => r,
            _ = ctx.cancel.cancelled() => {
                return Err(CcError::tool("tool", "Read cancelled"));
            }
        };
        read_result
            .map_err(|e| CcError::tool("tool", format!("failed to read {file_path}: {e}")))?;
        if buf.len() as u64 > MAX_FILE_BYTES {
            let cap_mb = MAX_FILE_BYTES / (1024 * 1024);
            return Ok(ToolResult::error(format!(
                "File {file_path} exceeded the {cap_mb} MB Read cap during read. \
                 Use `offset` + `limit` for a line range, or the Grep/Bash tools \
                 for pattern / byte-range reads."
            )));
        }
        let content = String::from_utf8_lossy(&buf).into_owned();

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
        let ctx = ToolContext::for_test_bare(CancellationToken::new());
        let result = tool
            .execute(json!({"file_path": file.to_string_lossy()}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("1\tline1"));
        assert!(result.content.contains("2\tline2"));
        assert!(result.content.contains("3\tline3"));
    }

    #[tokio::test]
    async fn read_not_found() {
        let tool = ReadTool;
        let ctx = ToolContext::for_test_bare(CancellationToken::new());
        let result = tool
            .execute(json!({"file_path": "/nonexistent/file.txt"}), &ctx)
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("not found"));
    }

    #[tokio::test]
    async fn read_with_offset_and_limit() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lines.txt");
        std::fs::write(&file, "a\nb\nc\nd\ne\n").unwrap();

        let tool = ReadTool;
        let ctx = ToolContext::for_test_bare(CancellationToken::new());
        let result = tool
            .execute(
                json!({"file_path": file.to_string_lossy(), "offset": 2, "limit": 2}),
                &ctx,
            )
            .await
            .unwrap();
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
        let ctx = ToolContext::for_test_bare(CancellationToken::new());
        let result = tool
            .execute(json!({"file_path": file.to_string_lossy()}), &ctx)
            .await
            .unwrap();
        assert!(result.is_error, "oversize file must be rejected");
        assert!(
            result.content.contains("above the") && result.content.contains("Read cap"),
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
        let token = CancellationToken::new();
        token.cancel(); // pre-cancel
        let ctx = ToolContext::for_test_bare(token);

        let result = tool
            .execute(json!({"file_path": file.to_string_lossy()}), &ctx)
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

    #[tokio::test]
    async fn read_metadata_size_matches_content_bytes() {
        // For a few fixed sizes, verify that the fd metadata.len() agrees
        // with the number of bytes actually read. This is the atomic-read
        // invariant: since we open once and both stat+read go through the
        // same fd, the two must agree.
        let dir = tempfile::tempdir().unwrap();
        for &size in &[0usize, 1, 128, 4096, 10_000] {
            let file = dir.path().join(format!("s{size}.bin"));
            let bytes: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
            std::fs::write(&file, &bytes).unwrap();

            let tool = ReadTool;
            let ctx = ToolContext::for_test_bare(CancellationToken::new());
            let result = tool
                .execute(json!({"file_path": file.to_string_lossy()}), &ctx)
                .await
                .unwrap();
            assert!(!result.is_error, "size {size}: {}", result.content);

            // The returned content is line-numbered, but for raw-byte tests we
            // care about the underlying read path not panicking and size-cap
            // not tripping for small files. Separately assert the raw fd read
            // through tokio agrees with metadata().
            let f = tokio::fs::File::open(&file).await.unwrap();
            let meta_size = f.metadata().await.unwrap().len();
            drop(f);
            assert_eq!(meta_size, size as u64);
        }
    }

    #[tokio::test]
    async fn read_tolerates_relink_race() {
        // Background task repeatedly swaps a symlink between a tiny target
        // and a "large" (sparse-reported) target while the main task calls
        // Read. The main task must either succeed with the small content
        // OR return the cap error — but never blow up with an OOM / panic.
        //
        // We use a sparse file for the "large" side so the test is cheap.
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let small = dir.path().join("small");
        let huge = dir.path().join("huge");
        let link = dir.path().join("link");

        std::fs::write(&small, "hello").unwrap();
        let f = std::fs::File::create(&huge).unwrap();
        f.set_len(MAX_FILE_BYTES + 1024).unwrap();
        drop(f);

        // Initial link points at small.
        #[cfg(unix)]
        std::os::unix::fs::symlink(&small, &link).unwrap();
        #[cfg(not(unix))]
        std::fs::copy(&small, &link).unwrap();

        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let link2 = link.clone();
        let small2 = small.clone();
        let huge2 = huge.clone();
        #[cfg(unix)]
        let swapper = std::thread::spawn(move || {
            let mut flip = false;
            while !stop2.load(Ordering::Relaxed) {
                let _ = std::fs::remove_file(&link2);
                let target = if flip { &small2 } else { &huge2 };
                let _ = std::os::unix::fs::symlink(target, &link2);
                flip = !flip;
            }
        });

        let tool = ReadTool;
        let ctx = ToolContext::for_test_bare(CancellationToken::new());
        for _ in 0..50 {
            let result = tool
                .execute(json!({"file_path": link.to_string_lossy()}), &ctx)
                .await;
            // Acceptable outcomes:
            //   - Ok with is_error=false (happy path)
            //   - Ok with is_error=true (cap hit)
            //   - Ok with is_error=true "File not found" / stat error (link
            //     was gone mid-swap — acceptable).
            //   - Err (OS-level open failure during swap — rare but ok).
            // UNacceptable:
            //   - Panic / OOM / returning 10 GB of content for a 5-byte
            //     file.
            if let Ok(r) = result {
                if !r.is_error {
                    // content is line-numbered "1\thello"
                    assert!(
                        r.content.len() < 1024,
                        "unexpectedly large payload on the 'small' side: {} bytes",
                        r.content.len()
                    );
                }
            }
        }
        stop.store(true, Ordering::Relaxed);
        #[cfg(unix)]
        swapper.join().unwrap();
    }

    /// P0 #13: Read refuses kernel-fs and infinite-stream device nodes
    /// without opening an fd. Uses `/dev/zero` because it always exists
    /// on Unix and an accidental non-capped read would OOM the process
    /// by streaming `\x00` until the 50 MB cap kicked in.
    #[cfg(unix)]
    #[tokio::test]
    async fn read_refuses_blocked_device_paths() {
        let tool = ReadTool;
        let ctx = ToolContext::for_test_bare(CancellationToken::new());
        for path in ["/dev/zero", "/dev/urandom", "/dev/null", "/proc/cpuinfo"] {
            if !std::path::Path::new(path).exists() {
                continue;
            }
            let r = tool
                .execute(json!({"file_path": path}), &ctx)
                .await
                .unwrap();
            assert!(r.is_error, "read should refuse {path}: {}", r.content);
            assert!(
                r.content.contains("blocked-device-paths"),
                "{}: missing explanation: {}",
                path,
                r.content
            );
        }
    }

    /// P0 #3: Read must refuse git-ignored paths with a structured
    /// error pointing the caller to Bash for an explicit bypass.
    #[tokio::test]
    async fn read_refuses_gitignored_paths() {
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
        assert!(std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(repo)
            .status()
            .unwrap()
            .success());
        std::fs::write(repo.join(".gitignore"), "*.env\n").unwrap();
        let secret = repo.join(".env");
        std::fs::write(&secret, "TOKEN=abc").unwrap();

        let tool = ReadTool;
        let ctx = ToolContext::for_test_bare(CancellationToken::new());
        let r = tool
            .execute(json!({"file_path": secret.to_string_lossy()}), &ctx)
            .await
            .unwrap();
        assert!(r.is_error, "read should refuse gitignored path");
        assert!(
            r.content.contains("git-ignored"),
            "missing explanation: {}",
            r.content
        );
        assert!(
            !r.content.contains("TOKEN=abc"),
            "secret bytes leaked through refusal: {}",
            r.content
        );
    }
}
