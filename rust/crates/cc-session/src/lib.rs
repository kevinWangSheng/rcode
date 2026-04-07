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
        let path = transcript_path(&id);
        fs::create_dir_all(path.parent().unwrap())
            .map_err(|e| CcError::Io(format!("failed to create session dir: {e}")))?;
        Ok(Session {
            id,
            transcript_path: path,
        })
    }

    /// Resume an existing session by ID.
    pub fn resume(id: &str) -> CcResult<(Self, Vec<MessageParam>)> {
        let path = transcript_path(id);
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
