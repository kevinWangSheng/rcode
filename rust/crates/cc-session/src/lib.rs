use cc_core::{CcError, CcResult, ContentBlock, MessageContent, MessageParam, Role};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

use cc_core::Usage;

/// A single entry in the JSONL transcript.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptEntry {
    pub message: MessageParam,
    pub timestamp: String,
    /// Token usage for this turn (only set on assistant messages).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// If true, marks a compaction boundary in the transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact_boundary: Option<bool>,
}

/// Session metadata stored in metadata.json alongside the transcript.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMetadata {
    pub model: String,
    pub started_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// Session metadata for listing.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub id: String,
    pub started_at: Option<String>,
    pub message_count: usize,
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
            .ok_or_else(|| CcError::io(format!("invalid transcript path: {}", path.display())))?;
        fs::create_dir_all(parent)
            .map_err(|e| CcError::io(format!("failed to create session dir: {e}")))?;
        Ok(Session {
            id,
            transcript_path: path,
        })
    }

    /// Resume an existing session by ID. Looks first in the Rust layout
    /// (`~/.claude/sessions/<id>/transcript.jsonl`); if missing, falls back to
    /// the TypeScript-version layout under `~/.claude/projects/<slug>/<id>.jsonl`
    /// and translates the on-disk schema into our `MessageParam`s.
    ///
    /// When a TS session is resumed, the `transcript_path` is set to the Rust
    /// layout path so subsequent appends create a fresh JSONL alongside the
    /// imported messages — we never write back into the TS file.
    pub fn resume(id: &str) -> CcResult<(Self, Vec<MessageParam>)> {
        let path = transcript_path(id)?;
        if path.exists() {
            let messages = load_transcript(&path)?;
            return Ok((
                Session {
                    id: id.to_string(),
                    transcript_path: path,
                },
                messages,
            ));
        }

        // TS-format fallback.
        if let Some(ts_path) = find_ts_session(id) {
            let messages = load_ts_transcript(&ts_path)?;
            // Make sure the new transcript dir exists so a follow-up `append`
            // can write the resumed session forward.
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).ok();
            }
            return Ok((
                Session {
                    id: id.to_string(),
                    transcript_path: path,
                },
                messages,
            ));
        }

        Err(CcError::NotFound(format!(
            "session not found: {id} (looked in {} and ~/.claude/projects/*/{id}.jsonl)",
            path.display()
        )))
    }

    /// Append a message to the JSONL transcript file.
    ///
    /// Each append is fsynced before returning so that a crash / SIGKILL /
    /// power loss after `append` returns is guaranteed to preserve the
    /// turn on disk. `RUST_REWRITE_PLAN.md` §3 promises per-turn
    /// durability; without `sync_all` the libc-level and OS page cache
    /// can silently drop the last write.
    pub fn append(&self, message: &MessageParam) -> CcResult<()> {
        self.append_entry(message, None, None)
    }

    /// Append a message with optional usage and compaction boundary.
    pub fn append_with_usage(
        &self,
        message: &MessageParam,
        usage: Option<Usage>,
        compact_boundary: Option<bool>,
    ) -> CcResult<()> {
        self.append_entry(message, usage, compact_boundary)
    }

    fn append_entry(
        &self,
        message: &MessageParam,
        usage: Option<Usage>,
        compact_boundary: Option<bool>,
    ) -> CcResult<()> {
        let entry = TranscriptEntry {
            message: message.clone(),
            timestamp: Utc::now().to_rfc3339(),
            usage,
            compact_boundary,
        };
        let line = serde_json::to_string(&entry).map_err(CcError::Json)?;

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.transcript_path)
            .map_err(|e| CcError::io(format!("failed to open transcript: {e}")))?;

        writeln!(file, "{line}")
            .map_err(|e| CcError::io(format!("failed to write transcript: {e}")))?;

        // Durability: flush libc buffers, then fsync so the write survives
        // a SIGKILL / power loss. If the FS cannot durably persist (disk
        // full, read-only remount, network FS outage), surface the error
        // so the caller can decide — silently continuing would mislead
        // resume logic into a false sense of persistence.
        file.flush()
            .map_err(|e| CcError::io(format!("failed to flush transcript: {e}")))?;
        file.sync_all()
            .map_err(|e| CcError::io(format!("failed to fsync transcript: {e}")))?;

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

    /// Path to the session directory.
    fn session_dir(&self) -> Option<&Path> {
        self.transcript_path.parent()
    }

    /// Write session metadata alongside the transcript.
    ///
    /// Atomically persisted via a same-directory `NamedTempFile` +
    /// `persist`. A crash (SIGKILL, power loss) between the tmpfile
    /// write and the rename cannot leave `metadata.json` truncated:
    /// readers either see the prior fully-written version or no file,
    /// never a half-written JSON document. See spec
    /// `session-persistence` Scenario "Metadata write is atomic".
    pub fn write_metadata(&self, metadata: &SessionMetadata) -> CcResult<()> {
        let Some(dir) = self.session_dir() else {
            return Err(CcError::io("no session directory"));
        };
        // Ensure the session directory exists before placing a tmpfile
        // inside it. `Session::new` creates it, but a `Session` constructed
        // directly in tests (or via resume of a TS-only session where the
        // Rust-layout dir has not been touched yet) may not have it.
        fs::create_dir_all(dir).map_err(|e| {
            CcError::io(format!(
                "failed to create session dir {}: {e}",
                dir.display()
            ))
        })?;
        let path = dir.join("metadata.json");
        let json = serde_json::to_string_pretty(metadata).map_err(CcError::Json)?;

        // Same-dir tempfile so the final `persist` is an atomic rename
        // on the same filesystem (cross-FS rename would fall back to
        // copy + unlink and lose atomicity).
        let tmp = tempfile::NamedTempFile::new_in(dir).map_err(|e| {
            CcError::io(format!(
                "failed to create metadata tempfile in {}: {e}",
                dir.display()
            ))
        })?;
        {
            let mut file = tmp.as_file();
            file.write_all(json.as_bytes()).map_err(|e| {
                CcError::io(format!(
                    "failed to write metadata tempfile {}: {e}",
                    path.display()
                ))
            })?;
            file.flush().map_err(|e| {
                CcError::io(format!(
                    "failed to flush metadata tempfile {}: {e}",
                    path.display()
                ))
            })?;
            file.sync_all().map_err(|e| {
                CcError::io(format!(
                    "failed to fsync metadata tempfile {}: {e}",
                    path.display()
                ))
            })?;
        }

        tmp.persist(&path).map_err(|e| {
            CcError::io(format!(
                "failed to persist metadata to {}: {e}",
                path.display()
            ))
        })?;
        Ok(())
    }

    /// Load session metadata.
    pub fn load_metadata(&self) -> CcResult<SessionMetadata> {
        let Some(dir) = self.session_dir() else {
            return Err(CcError::io("no session directory"));
        };
        let path = dir.join("metadata.json");
        let content = fs::read_to_string(&path)
            .map_err(|e| CcError::io(format!("failed to read metadata: {e}")))?;
        serde_json::from_str(&content).map_err(CcError::Json)
    }
}

fn sessions_root() -> CcResult<PathBuf> {
    let home = dirs::home_dir()
        .ok_or_else(|| CcError::io("could not determine home directory for session storage"))?;
    if home.as_os_str().is_empty() {
        return Err(CcError::io(
            "home directory is empty; cannot locate session storage",
        ));
    }
    Ok(home.join(".claude").join("sessions"))
}

fn transcript_path(id: &str) -> CcResult<PathBuf> {
    Ok(sessions_root()?.join(id).join("transcript.jsonl"))
}

fn load_transcript(path: &Path) -> CcResult<Vec<MessageParam>> {
    let file = File::open(path)
        .map_err(|e| CcError::io(format!("failed to open transcript {}: {e}", path.display())))?;

    let reader = BufReader::new(file);
    let mut messages = Vec::new();

    // Transcripts are append-only and crash-fragile: a process killed mid-
    // write can leave a half-flushed final line. Matching `load_ts_transcript`,
    // we skip unparseable lines with a warning rather than abort the resume —
    // a partial history is strictly better than no history.
    for (line_num, line) in reader.lines().enumerate() {
        let line = line
            .map_err(|e| CcError::io(format!("failed to read transcript line {line_num}: {e}")))?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<TranscriptEntry>(&line) {
            Ok(entry) => messages.push(entry.message),
            Err(e) => {
                tracing::warn!(
                    "skipping malformed transcript line {} in {}: {e}",
                    line_num + 1,
                    path.display()
                );
            }
        }
    }

    Ok(messages)
}

/// Look for a TypeScript-version session file at
/// `~/.claude/projects/<slug>/<id>.jsonl`. Returns the first match.
fn find_ts_session(id: &str) -> Option<PathBuf> {
    let projects = dirs::home_dir()?.join(".claude").join("projects");
    if !projects.is_dir() {
        return None;
    }
    let target = format!("{id}.jsonl");
    for entry in fs::read_dir(&projects).ok()?.flatten() {
        let slug_dir = entry.path();
        if !slug_dir.is_dir() {
            continue;
        }
        let candidate = slug_dir.join(&target);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Parse the TypeScript-version transcript JSONL schema into `MessageParam`s.
///
/// Each line has the shape:
/// ```json
/// {"type":"user"|"assistant","message":{"role":"user","content":"..."|[...]}}
/// ```
///
/// Non-message entries (`type` outside user/assistant) are skipped. Content may
/// be either a plain string or an array of content blocks (text / tool_use /
/// tool_result). Blocks that fail to parse as a known `ContentBlock` variant are
/// skipped rather than failing the whole load — we prefer a partial replay over
/// refusing to resume.
fn load_ts_transcript(path: &Path) -> CcResult<Vec<MessageParam>> {
    let file = File::open(path).map_err(|e| {
        CcError::io(format!(
            "failed to open ts transcript {}: {e}",
            path.display()
        ))
    })?;
    let reader = BufReader::new(file);
    let mut messages = Vec::new();

    for (line_num, line) in reader.lines().enumerate() {
        let line = line.map_err(|e| {
            CcError::io(format!("failed to read ts transcript line {line_num}: {e}"))
        })?;
        if line.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue, // tolerate noise lines
        };

        let entry_type = value.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if entry_type != "user" && entry_type != "assistant" {
            continue;
        }

        let Some(message) = value.get("message") else {
            continue;
        };
        let role = match message.get("role").and_then(|v| v.as_str()) {
            Some("user") => Role::User,
            Some("assistant") => Role::Assistant,
            _ => continue,
        };

        let content = match message.get("content") {
            Some(serde_json::Value::String(s)) => MessageContent::Text(s.clone()),
            Some(serde_json::Value::Array(arr)) => {
                let mut blocks: Vec<ContentBlock> = Vec::new();
                for block in arr {
                    if let Ok(cb) = serde_json::from_value::<ContentBlock>(block.clone()) {
                        blocks.push(cb);
                        continue;
                    }
                    // Fallback: extract plain text if the object has a `text` field.
                    if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                        blocks.push(ContentBlock::text(text));
                    }
                }
                if blocks.is_empty() {
                    continue;
                }
                MessageContent::Blocks(blocks)
            }
            _ => continue,
        };

        messages.push(MessageParam { role, content });
    }

    Ok(messages)
}

/// List session infos with metadata.
pub fn list_session_infos() -> Vec<SessionInfo> {
    let base = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join("sessions");

    if !base.exists() {
        return Vec::new();
    }

    fs::read_dir(&base)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| {
            let id = e.file_name().to_string_lossy().to_string();
            let transcript = e.path().join("transcript.jsonl");
            let message_count = if transcript.exists() {
                fs::read_to_string(&transcript)
                    .map(|c| c.lines().filter(|l| !l.trim().is_empty()).count())
                    .unwrap_or(0)
            } else {
                0
            };
            let started_at = e
                .metadata()
                .ok()
                .and_then(|m| m.created().ok())
                .map(|t| chrono::DateTime::<Utc>::from(t).to_rfc3339());
            SessionInfo {
                id,
                started_at,
                message_count,
            }
        })
        .collect()
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
    use tempfile::tempdir;

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
        let tmp = tempdir().unwrap();
        let _hg = HomeGuard::set(tmp.path());

        let session = Session::new().expect("Session::new should succeed in tempdir HOME");
        let msg = MessageParam::user("hello");
        session.append(&msg).expect("append should succeed");

        let loaded = session.load_messages().expect("load_messages");
        assert_eq!(loaded.len(), 1);
    }

    #[test]
    fn ts_transcript_parses_string_content() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sess.jsonl");
        let mut f = File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"type":"user","message":{{"role":"user","content":"hello"}}}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"type":"assistant","message":{{"role":"assistant","content":"hi back"}}}}"#
        )
        .unwrap();

        let messages = load_ts_transcript(&path).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, Role::User);
        assert!(matches!(&messages[0].content, MessageContent::Text(s) if s == "hello"));
        assert_eq!(messages[1].role, Role::Assistant);
    }

    #[test]
    fn ts_transcript_parses_block_content_and_skips_non_messages() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sess.jsonl");
        let mut f = File::create(&path).unwrap();
        // Non-message entries that should be skipped
        writeln!(f, r#"{{"type":"summary","text":"ignore me"}}"#).unwrap();
        writeln!(
            f,
            r#"{{"type":"system","message":{{"role":"system","content":"x"}}}}"#
        )
        .unwrap();
        // Real assistant turn with blocks
        writeln!(
            f,
            r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"answer"}},{{"type":"tool_use","id":"t1","name":"Bash","input":{{}}}}]}}}}"#
        )
        .unwrap();
        // Tolerate a noise line
        writeln!(f, "not json at all").unwrap();

        let messages = load_ts_transcript(&path).unwrap();
        assert_eq!(messages.len(), 1);
        let MessageContent::Blocks(blocks) = &messages[0].content else {
            panic!("expected blocks");
        };
        assert_eq!(blocks.len(), 2);
        assert!(matches!(&blocks[0], ContentBlock::Text(t) if t.text == "answer"));
        assert!(matches!(&blocks[1], ContentBlock::ToolUse(_)));
    }

    #[test]
    fn metadata_roundtrip() {
        let dir = tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        let session = Session {
            id: "test-meta".into(),
            transcript_path: transcript,
        };
        let meta = SessionMetadata {
            model: "claude-sonnet-4-6".into(),
            started_at: "2026-04-09T12:00:00Z".into(),
            project_path: Some("/home/user/project".into()),
            cwd: None,
        };
        session.write_metadata(&meta).unwrap();
        let loaded = session.load_metadata().unwrap();
        assert_eq!(loaded.model, "claude-sonnet-4-6");
        assert_eq!(loaded.project_path.as_deref(), Some("/home/user/project"));
    }

    #[test]
    fn rust_layout_roundtrip_append_and_load() {
        let dir = tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        let session = Session {
            id: "test".into(),
            transcript_path: transcript.clone(),
        };
        session.append(&MessageParam::user("one")).unwrap();
        session.append(&MessageParam::user("two")).unwrap();
        let loaded = session.load_messages().unwrap();
        assert_eq!(loaded.len(), 2);
    }

    #[test]
    fn native_transcript_tolerates_malformed_tail() {
        // Simulates a process that crashed mid-write: two clean entries
        // followed by a partial/invalid tail line. The old behavior failed
        // the whole load; the robust loader skips the bad line and keeps
        // the rest so the user's session can still resume.
        let dir = tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let mut f = File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"message":{{"role":"user","content":"first"}},"timestamp":"2026-04-17T00:00:00Z"}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"message":{{"role":"assistant","content":"second"}},"timestamp":"2026-04-17T00:00:01Z"}}"#
        )
        .unwrap();
        // Partial / invalid JSON — crashed mid-flush.
        writeln!(
            f,
            "{{\"message\":{{\"role\":\"user\",\"content\":\"# broken trailing line"
        )
        .unwrap();
        // Another bit of noise for good measure.
        writeln!(f, "not json").unwrap();

        let messages = load_transcript(&path).unwrap();
        assert_eq!(messages.len(), 2, "valid prefix must survive a bad tail");
        assert_eq!(messages[0].role, Role::User);
        assert_eq!(messages[1].role, Role::Assistant);
    }

    #[test]
    fn native_transcript_empty_file_returns_no_messages() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("empty.jsonl");
        File::create(&path).unwrap();
        let messages = load_transcript(&path).unwrap();
        assert!(messages.is_empty());
    }
}
