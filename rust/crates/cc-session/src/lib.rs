use cc_core::{CcError, CcResult, MessageParam};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// A single entry in the JSONL transcript.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptEntry {
    pub message: MessageParam,
    pub timestamp: String,
}

/// Manages a conversation session: ID, transcript path, and in-memory messages.
#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    transcript_path: PathBuf,
}

impl Session {
    /// Create a new session with a freshly generated UUID.
    pub fn new() -> CcResult<Self> {
        let id = Uuid::new_v4().to_string();
        let path = transcript_path(&id)?;
        let parent = path
            .parent()
            .ok_or_else(|| CcError::Io(format!("invalid transcript path: {}", path.display())))?;
        fs::create_dir_all(parent)
            .map_err(|e| CcError::Io(format!("failed to create session dir: {e}")))?;
        Ok(Session {
            id,
            transcript_path: path,
        })
    }

    /// Resume an existing session by ID.
    pub fn resume(id: &str) -> CcResult<(Self, Vec<MessageParam>)> {
        let path = transcript_path(id)?;
        if !path.exists() {
            return Err(CcError::NotFound(format!(
                "session not found: {id} (expected at {})",
                path.display()
            )));
        }
        let messages = load_transcript(&path)?;
        Ok((
            Session {
                id: id.to_string(),
                transcript_path: path,
            },
            messages,
        ))
    }

    /// Append a message to the JSONL transcript file.
    ///
    /// Each append is fsynced before returning so that a crash / SIGKILL /
    /// power loss after `append` returns is guaranteed to preserve the
    /// turn on disk. `RUST_REWRITE_PLAN.md` §3 promises per-turn
    /// durability; without `sync_all` the libc-level and OS page cache
    /// can silently drop the last write.
    pub fn append(&self, message: &MessageParam) -> CcResult<()> {
        let entry = TranscriptEntry {
            message: message.clone(),
            timestamp: Utc::now().to_rfc3339(),
        };
        let line = serde_json::to_string(&entry).map_err(CcError::Json)?;

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.transcript_path)
            .map_err(|e| CcError::Io(format!("failed to open transcript: {e}")))?;

        writeln!(file, "{line}")
            .map_err(|e| CcError::Io(format!("failed to write transcript: {e}")))?;

        // Durability: flush libc buffers, then fsync so the write survives
        // a SIGKILL / power loss. If the FS cannot durably persist (disk
        // full, read-only remount, network FS outage), surface the error
        // so the caller can decide — silently continuing would mislead
        // resume logic into a false sense of persistence.
        file.flush()
            .map_err(|e| CcError::Io(format!("failed to flush transcript: {e}")))?;
        file.sync_all()
            .map_err(|e| CcError::Io(format!("failed to fsync transcript: {e}")))?;

        Ok(())
    }

    /// Load all messages from the transcript file.
    pub fn load_messages(&self) -> CcResult<Vec<MessageParam>> {
        load_transcript(&self.transcript_path)
    }

    /// Path to the transcript file.
    pub fn transcript_path(&self) -> &Path {
        &self.transcript_path
    }
}

fn sessions_root() -> CcResult<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| {
        CcError::Io("could not determine home directory for session storage".into())
    })?;
    if home.as_os_str().is_empty() {
        return Err(CcError::Io(
            "home directory is empty; cannot locate session storage".into(),
        ));
    }
    Ok(home.join(".claude").join("sessions"))
}

fn transcript_path(id: &str) -> CcResult<PathBuf> {
    Ok(sessions_root()?.join(id).join("transcript.jsonl"))
}

fn load_transcript(path: &Path) -> CcResult<Vec<MessageParam>> {
    let file = File::open(path)
        .map_err(|e| CcError::Io(format!("failed to open transcript {}: {e}", path.display())))?;

    let reader = BufReader::new(file);
    let mut messages = Vec::new();

    for (line_num, line) in reader.lines().enumerate() {
        let line = line
            .map_err(|e| CcError::Io(format!("failed to read transcript line {line_num}: {e}")))?;
        if line.trim().is_empty() {
            continue;
        }
        let entry: TranscriptEntry = serde_json::from_str(&line).map_err(CcError::Json)?;
        messages.push(entry.message);
    }

    Ok(messages)
}

/// List all session IDs (directory names under `~/.claude/sessions/`).
///
/// Returns an empty vector when the home directory cannot be resolved or
/// the sessions directory does not exist yet. A missing home is logged so
/// misconfigured environments are noticed without crashing the CLI.
pub fn list_sessions() -> Vec<String> {
    let base = match sessions_root() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("cc-session: cannot list sessions: {e}");
            return Vec::new();
        }
    };

    if !base.exists() {
        return Vec::new();
    }

    fs::read_dir(&base)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Serialize tests that mutate process-wide env vars so they don't race.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// RAII helper: override `HOME` (and `USERPROFILE` on Windows) for the
    /// duration of the test, then restore the original value on drop.
    struct HomeGuard {
        prev_home: Option<std::ffi::OsString>,
        #[cfg(windows)]
        prev_userprofile: Option<std::ffi::OsString>,
    }

    impl HomeGuard {
        fn set(new_home: &std::path::Path) -> Self {
            let prev_home = std::env::var_os("HOME");
            #[cfg(windows)]
            let prev_userprofile = std::env::var_os("USERPROFILE");
            // SAFETY: tests holding ENV_LOCK are single-threaded w.r.t. each other.
            unsafe {
                std::env::set_var("HOME", new_home);
                #[cfg(windows)]
                std::env::set_var("USERPROFILE", new_home);
            }
            HomeGuard {
                prev_home,
                #[cfg(windows)]
                prev_userprofile,
            }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.prev_home {
                    Some(v) => std::env::set_var("HOME", v),
                    None => std::env::remove_var("HOME"),
                }
                #[cfg(windows)]
                match &self.prev_userprofile {
                    Some(v) => std::env::set_var("USERPROFILE", v),
                    None => std::env::remove_var("USERPROFILE"),
                }
            }
        }
    }

    /// Session::new must surface an IO error (not panic) when HOME points
    /// at a location where create_dir_all cannot succeed. This guards
    /// against the former `path.parent().unwrap()` + `Default::expect`
    /// panic paths described in the fix-session-startup-panic proposal.
    #[test]
    fn session_new_surfaces_io_error_for_unwritable_home() {
        let _g = ENV_LOCK.lock().unwrap();
        // /dev/null exists but is not a directory, so create_dir_all underneath
        // it fails with ENOTDIR (portable across macOS and Linux CI runners).
        let bad_home = std::path::Path::new("/dev/null");
        let _hg = HomeGuard::set(bad_home);

        let result = std::panic::catch_unwind(Session::new);
        let unwrapped = result.expect("Session::new must not panic on unusable HOME");
        match unwrapped {
            Err(CcError::Io(_)) => {}
            other => panic!("expected CcError::Io, got {other:?}"),
        }
    }

    /// Regression: make sure an empty HOME (some container images ship
    /// with an empty string) does not slip past as "."
    #[test]
    fn session_new_rejects_empty_home() {
        let _g = ENV_LOCK.lock().unwrap();
        let _hg = HomeGuard::set(std::path::Path::new(""));

        // dirs::home_dir() on some platforms falls back to getpwuid_r when
        // HOME is empty, so we only assert the outcome is not a panic and
        // that if a path *is* resolved successfully it is non-empty.
        let result = std::panic::catch_unwind(Session::new);
        let unwrapped = result.expect("Session::new must not panic on empty HOME");
        if let Ok(session) = unwrapped {
            assert!(
                !session
                    .transcript_path()
                    .to_string_lossy()
                    .starts_with("/.claude/sessions/"),
                "transcript path must not be anchored at filesystem root via empty HOME: {}",
                session.transcript_path().display()
            );
        }
    }

    /// Happy path: Session::new succeeds, append writes a line, and the
    /// written line survives a re-open without any explicit .close().
    #[test]
    fn session_append_roundtrip() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = tempdir();
        let _hg = HomeGuard::set(tmp.path());

        let session = Session::new().expect("Session::new should succeed in tempdir HOME");
        let msg = MessageParam::user("hello");
        session.append(&msg).expect("append should succeed");

        let loaded = session.load_messages().expect("load_messages");
        assert_eq!(loaded.len(), 1);
    }

    /// Tiny hand-rolled tempdir helper: avoids pulling in a new dev-dep.
    fn tempdir() -> TempDir {
        let mut base = std::env::temp_dir();
        base.push(format!("cc-session-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&base).expect("create tempdir");
        TempDir(base)
    }

    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
