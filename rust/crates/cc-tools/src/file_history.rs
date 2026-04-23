//! Shared helpers for the file-history snapshot producers (Edit / Write).
//!
//! Lives in its own module so the `MAX_SNAPSHOT_BYTES` threshold and the
//! `relpath` helper are defined in exactly one place. See openspec
//! `fix-file-history-snapshot-producers` for the full rationale.

use std::path::Path;

/// Size threshold above which the snapshot path emits a `tracing::warn!`
/// but still persists. 64 MiB is loose enough for any real source-code
/// file yet tight enough to flag a 500 MiB accidental binary write.
pub(crate) const MAX_SNAPSHOT_BYTES: usize = 64 * 1024 * 1024;

/// Convert an absolute `path` into a project-relative string when it
/// lives under the current working directory; otherwise return the
/// absolute path. Mirrors TS `maybeShortenFilePath` in
/// `src/utils/fileHistory.ts`.
pub(crate) fn project_relative_path(path: &Path) -> String {
    let abs = path.to_string_lossy().into_owned();
    let Ok(cwd) = std::env::current_dir() else {
        return abs;
    };
    if let Ok(rel) = path.strip_prefix(&cwd) {
        // `strip_prefix` produces an empty path for exact cwd match; fall
        // back to the absolute form in that degenerate case.
        let rel_str = rel.to_string_lossy();
        if rel_str.is_empty() {
            abs
        } else {
            rel_str.into_owned()
        }
    } else {
        abs
    }
}
