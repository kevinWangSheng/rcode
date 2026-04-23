//! File-history snapshot types (parity with TS `src/utils/fileHistory.ts:33-52`).
//!
//! These types were historically defined in `cc_session::lib`, but the
//! `cc_core::SessionSink` trait needs to reference `FileHistorySnapshot` in
//! its method signature — and moving the struct here lets `cc-core` define
//! the sink contract without pulling `cc-session` into its dep graph.
//!
//! `cc-session` re-exports these at their historical path so external
//! callers (including this workspace's tests) keep compiling unchanged.
//!
//! TS wire parity: camelCase field names, same field set — a Rust-written
//! transcript is a drop-in replacement for a TS-written one and vice versa.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One backup entry for a tracked file. A `backup_file_name` of `None`
/// represents "the file did not exist at this version" (parity with TS
/// `BackupFileName = string | null`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FileHistoryBackup {
    pub backup_file_name: Option<String>,
    pub version: u32,
    /// ISO-8601 timestamp (TS persists `Date` as an ISO string).
    pub backup_time: String,
}

/// Per-message snapshot of every file Claude touched up to that turn.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FileHistorySnapshot {
    /// UUID of the message this snapshot belongs to.
    pub message_id: String,
    /// Map of absolute file path → backup slot.
    pub tracked_file_backups: BTreeMap<String, FileHistoryBackup>,
    /// ISO-8601 timestamp.
    pub timestamp: String,
}

/// JSONL wrapper around a `FileHistorySnapshot`. This is what ends up on
/// disk as one `{"type":"file-history-snapshot", ...}` line.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FileHistorySnapshotMessage {
    pub message_id: String,
    pub snapshot: FileHistorySnapshot,
    /// `true` when this snapshot refines an earlier one for the same
    /// message (matches TS `isSnapshotUpdate`).
    pub is_snapshot_update: bool,
}
