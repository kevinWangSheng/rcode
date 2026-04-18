//! TodoList — in-memory task tracking list for the agent's work plan.
//!
//! This is the todo/task-list system used by TaskCreate/TaskUpdate/TaskList/TaskGet tools.
//! Distinct from cc-agents TaskRegistry, which manages background async tasks.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// Status of a todo task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Deleted,
}

impl std::fmt::Display for TodoStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TodoStatus::Pending => write!(f, "pending"),
            TodoStatus::InProgress => write!(f, "in_progress"),
            TodoStatus::Completed => write!(f, "completed"),
            TodoStatus::Deleted => write!(f, "deleted"),
        }
    }
}

/// A single task in the todo list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoTask {
    pub id: String,
    pub subject: String,
    pub description: String,
    pub status: TodoStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_form: Option<String>,
    /// IDs of tasks this task blocks (they cannot start until this completes).
    #[serde(default)]
    pub blocks: Vec<String>,
    /// IDs of tasks that must complete before this task can start.
    #[serde(default)]
    pub blocked_by: Vec<String>,
    /// Arbitrary metadata.
    #[serde(default)]
    pub metadata: HashMap<String, Value>,
}

/// In-memory list of todo tasks with sequential integer IDs.
#[derive(Debug, Default)]
pub struct TodoList {
    tasks: HashMap<String, TodoTask>,
    next_id: u64,
}

impl TodoList {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a new task. Returns its ID.
    pub fn create(
        &mut self,
        subject: String,
        description: String,
        active_form: Option<String>,
        metadata: Option<HashMap<String, Value>>,
    ) -> String {
        self.next_id += 1;
        let id = self.next_id.to_string();
        let task = TodoTask {
            id: id.clone(),
            subject,
            description,
            status: TodoStatus::Pending,
            owner: None,
            active_form,
            blocks: Vec::new(),
            blocked_by: Vec::new(),
            metadata: metadata.unwrap_or_default(),
        };
        self.tasks.insert(id.clone(), task);
        id
    }

    /// Get a task by ID.
    pub fn get(&self, id: &str) -> Option<&TodoTask> {
        self.tasks.get(id)
    }

    /// List all non-deleted tasks in insertion order (by numeric ID).
    pub fn list(&self) -> Vec<&TodoTask> {
        let mut tasks: Vec<&TodoTask> = self
            .tasks
            .values()
            .filter(|t| t.status != TodoStatus::Deleted)
            .collect();
        // Sort by numeric ID for stable output
        tasks.sort_by(|a, b| {
            let ai: u64 = a.id.parse().unwrap_or(0);
            let bi: u64 = b.id.parse().unwrap_or(0);
            ai.cmp(&bi)
        });
        tasks
    }

    /// Update a task. Returns an error string if the task does not exist.
    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &mut self,
        id: &str,
        status: Option<TodoStatus>,
        subject: Option<String>,
        description: Option<String>,
        active_form: Option<String>,
        owner: Option<String>,
        add_blocks: Option<Vec<String>>,
        add_blocked_by: Option<Vec<String>>,
        metadata_patch: Option<HashMap<String, Value>>,
    ) -> Result<(), String> {
        let task = self
            .tasks
            .get_mut(id)
            .ok_or_else(|| format!("task {id} not found"))?;

        if let Some(s) = status {
            task.status = s;
        }
        if let Some(s) = subject {
            task.subject = s;
        }
        if let Some(d) = description {
            task.description = d;
        }
        if let Some(af) = active_form {
            task.active_form = Some(af);
        }
        if let Some(o) = owner {
            task.owner = Some(o);
        }
        if let Some(blocks) = add_blocks {
            for b in blocks {
                if !task.blocks.contains(&b) {
                    task.blocks.push(b);
                }
            }
        }
        if let Some(blocked_by) = add_blocked_by {
            for b in blocked_by {
                if !task.blocked_by.contains(&b) {
                    task.blocked_by.push(b);
                }
            }
        }
        if let Some(patch) = metadata_patch {
            for (k, v) in patch {
                if v.is_null() {
                    task.metadata.remove(&k);
                } else {
                    task.metadata.insert(k, v);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_get() {
        let mut list = TodoList::new();
        let id = list.create("Fix bug".into(), "Desc".into(), None, None);
        let task = list.get(&id).unwrap();
        assert_eq!(task.subject, "Fix bug");
        assert_eq!(task.status, TodoStatus::Pending);
        assert_eq!(task.id, "1");
    }

    #[test]
    fn list_excludes_deleted() {
        let mut list = TodoList::new();
        let id1 = list.create("Task 1".into(), "".into(), None, None);
        let id2 = list.create("Task 2".into(), "".into(), None, None);
        list.update(
            &id1,
            Some(TodoStatus::Deleted),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let tasks = list.list();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, id2);
    }

    #[test]
    fn update_status_and_owner() {
        let mut list = TodoList::new();
        let id = list.create("Task".into(), "Desc".into(), None, None);
        list.update(
            &id,
            Some(TodoStatus::InProgress),
            None,
            None,
            None,
            Some("alice".into()),
            None,
            None,
            None,
        )
        .unwrap();
        let task = list.get(&id).unwrap();
        assert_eq!(task.status, TodoStatus::InProgress);
        assert_eq!(task.owner.as_deref(), Some("alice"));
    }

    #[test]
    fn update_blocks_and_blocked_by() {
        let mut list = TodoList::new();
        let id1 = list.create("Task 1".into(), "".into(), None, None);
        let id2 = list.create("Task 2".into(), "".into(), None, None);
        list.update(
            &id2,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(vec![id1.clone()]),
            None,
        )
        .unwrap();
        let task2 = list.get(&id2).unwrap();
        assert!(task2.blocked_by.contains(&id1));
    }

    #[test]
    fn update_metadata_patch() {
        let mut list = TodoList::new();
        let id = list.create("Task".into(), "".into(), None, None);
        let mut patch = HashMap::new();
        patch.insert("key1".to_string(), Value::String("val1".into()));
        list.update(&id, None, None, None, None, None, None, None, Some(patch))
            .unwrap();
        let task = list.get(&id).unwrap();
        assert_eq!(
            task.metadata.get("key1"),
            Some(&Value::String("val1".into()))
        );

        // Delete key via null
        let mut del_patch = HashMap::new();
        del_patch.insert("key1".to_string(), Value::Null);
        list.update(
            &id,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(del_patch),
        )
        .unwrap();
        let task = list.get(&id).unwrap();
        assert!(!task.metadata.contains_key("key1"));
    }

    #[test]
    fn sequential_ids() {
        let mut list = TodoList::new();
        let ids: Vec<String> = (0..5)
            .map(|i| list.create(format!("Task {i}"), "".into(), None, None))
            .collect();
        assert_eq!(ids, vec!["1", "2", "3", "4", "5"]);
    }
}
