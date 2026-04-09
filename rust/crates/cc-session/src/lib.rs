use cc_core::{CcError, CcResult, ContentBlock, MessageContent, MessageParam, Role};
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
        let path = transcript_path(&id);
        fs::create_dir_all(path.parent().unwrap())
            .map_err(|e| CcError::Io(format!("failed to create session dir: {e}")))?;
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
        let path = transcript_path(id);
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
            transcript_path(id).display()
        )))
    }

    /// Append a message to the JSONL transcript file.
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

impl Default for Session {
    fn default() -> Self {
        Session::new().expect("failed to create default session")
    }
}

fn transcript_path(id: &str) -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join("sessions")
        .join(id)
        .join("transcript.jsonl")
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
    let file = File::open(path)
        .map_err(|e| CcError::Io(format!("failed to open ts transcript {}: {e}", path.display())))?;
    let reader = BufReader::new(file);
    let mut messages = Vec::new();

    for (line_num, line) in reader.lines().enumerate() {
        let line = line.map_err(|e| {
            CcError::Io(format!("failed to read ts transcript line {line_num}: {e}"))
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

/// List all session IDs (directory names under `~/.claude/sessions/`).
pub fn list_sessions() -> Vec<String> {
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
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

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
        writeln!(f, r#"{{"type":"system","message":{{"role":"system","content":"x"}}}}"#).unwrap();
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
}
