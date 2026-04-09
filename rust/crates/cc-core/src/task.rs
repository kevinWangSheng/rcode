use serde::{Deserialize, Serialize};

/// The 4 stable task types (from Decision 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    LocalBash,
    LocalAgent,
    InProcessTeammate,
    RemoteAgent,
}

/// Task lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

/// Unique task identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskId(pub String);

impl TaskId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }
}

impl Default for TaskId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Base state shared by all task types.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskStateBase {
    pub id: TaskId,
    pub kind: TaskKind,
    pub status: TaskStatus,
    pub description: String,
    pub is_backgrounded: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Last output lines (for status display).
    #[serde(default)]
    pub output_tail: Vec<String>,
}

/// Task notification sent to the parent agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskNotification {
    pub task_id: TaskId,
    pub status: TaskStatus,
    pub summary: Option<String>,
}

/// Task output returned upon completion.
#[derive(Debug, Clone)]
pub struct TaskOutput {
    pub summary: String,
    pub content: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_id_generates_uuid() {
        let id = TaskId::new();
        assert!(!id.0.is_empty());
        // UUID v4 format: 8-4-4-4-12 hex chars
        assert_eq!(id.0.len(), 36);
    }

    #[test]
    fn task_kind_serialization() {
        assert_eq!(
            serde_json::to_string(&TaskKind::LocalBash).unwrap(),
            "\"local_bash\""
        );
        assert_eq!(
            serde_json::to_string(&TaskKind::InProcessTeammate).unwrap(),
            "\"in_process_teammate\""
        );
    }

    #[test]
    fn task_status_serialization() {
        assert_eq!(
            serde_json::to_string(&TaskStatus::Running).unwrap(),
            "\"running\""
        );
        assert_eq!(
            serde_json::to_string(&TaskStatus::Cancelled).unwrap(),
            "\"cancelled\""
        );
    }
}
