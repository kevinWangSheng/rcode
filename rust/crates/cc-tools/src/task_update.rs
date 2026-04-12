//! TaskUpdate tool — update a task in the todo list.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cc_core::CcResult;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::todo::{TodoList, TodoStatus};
use crate::{Tool, ToolInputSchema, ToolResult};

pub struct TaskUpdateTool {
    pub list: Arc<Mutex<TodoList>>,
}

fn parse_status(s: &str) -> Option<TodoStatus> {
    match s {
        "pending" => Some(TodoStatus::Pending),
        "in_progress" => Some(TodoStatus::InProgress),
        "completed" => Some(TodoStatus::Completed),
        "deleted" => Some(TodoStatus::Deleted),
        _ => None,
    }
}

#[async_trait]
impl Tool for TaskUpdateTool {
    fn name(&self) -> &str {
        "TaskUpdate"
    }

    fn description(&self) -> &str {
        "Update a task in the task list. Use to mark tasks in_progress, completed, or deleted; \
         change subject/description; assign owners; set up blocks/blockedBy dependencies; or \
         merge metadata. Read task state with TaskGet before updating to avoid staleness."
    }

    fn input_schema(&self) -> ToolInputSchema {
        serde_json::from_value(json!({
            "type": "object",
            "properties": {
                "taskId": {
                    "type": "string",
                    "description": "The ID of the task to update"
                },
                "status": {
                    "type": "string",
                    "enum": ["pending", "in_progress", "completed", "deleted"],
                    "description": "New status for the task"
                },
                "subject": {
                    "type": "string",
                    "description": "New subject for the task"
                },
                "description": {
                    "type": "string",
                    "description": "New description for the task"
                },
                "activeForm": {
                    "type": "string",
                    "description": "Present continuous form shown in spinner when in_progress"
                },
                "owner": {
                    "type": "string",
                    "description": "New owner for the task"
                },
                "addBlocks": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Task IDs that this task blocks"
                },
                "addBlockedBy": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Task IDs that block this task"
                },
                "metadata": {
                    "type": "object",
                    "description": "Metadata keys to merge into the task. Set a key to null to delete it.",
                    "additionalProperties": true
                }
            },
            "required": ["taskId"]
        }))
        .unwrap()
    }

    async fn execute(&self, input: Value, _cancel: &CancellationToken) -> CcResult<ToolResult> {
        let task_id = match input.get("taskId").and_then(Value::as_str) {
            Some(s) => s.to_string(),
            None => return Ok(ToolResult::error("missing required field: taskId")),
        };

        let status = input
            .get("status")
            .and_then(Value::as_str)
            .and_then(parse_status);

        let subject = input.get("subject").and_then(Value::as_str).map(str::to_string);
        let description = input.get("description").and_then(Value::as_str).map(str::to_string);
        let active_form = input.get("activeForm").and_then(Value::as_str).map(str::to_string);
        let owner = input.get("owner").and_then(Value::as_str).map(str::to_string);

        let add_blocks: Option<Vec<String>> = input
            .get("addBlocks")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().filter_map(Value::as_str).map(str::to_string).collect());

        let add_blocked_by: Option<Vec<String>> = input
            .get("addBlockedBy")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().filter_map(Value::as_str).map(str::to_string).collect());

        let metadata_patch: Option<HashMap<String, Value>> = input
            .get("metadata")
            .and_then(Value::as_object)
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect());

        let mut updated_fields: Vec<&str> = Vec::new();
        if status.is_some() { updated_fields.push("status"); }
        if subject.is_some() { updated_fields.push("subject"); }
        if description.is_some() { updated_fields.push("description"); }
        if active_form.is_some() { updated_fields.push("activeForm"); }
        if owner.is_some() { updated_fields.push("owner"); }
        if add_blocks.is_some() { updated_fields.push("blocks"); }
        if add_blocked_by.is_some() { updated_fields.push("blockedBy"); }
        if metadata_patch.is_some() { updated_fields.push("metadata"); }

        let result = {
            let mut list = self.list.lock().unwrap();
            list.update(
                &task_id,
                status,
                subject,
                description,
                active_form,
                owner,
                add_blocks,
                add_blocked_by,
                metadata_patch,
            )
        };

        match result {
            Ok(()) => {
                let msg = format!("Updated task #{task_id} {}", updated_fields.join(", "));
                Ok(ToolResult::ok(msg))
            }
            Err(e) => Ok(ToolResult::error(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task_create::TaskCreateTool;

    fn make_list() -> Arc<Mutex<TodoList>> {
        Arc::new(Mutex::new(TodoList::new()))
    }

    fn cancel() -> CancellationToken {
        CancellationToken::new()
    }

    #[tokio::test]
    async fn update_status_to_in_progress() {
        let list = make_list();
        let create = TaskCreateTool { list: list.clone() };
        create.execute(json!({"subject": "T", "description": ""}), &cancel()).await.unwrap();

        let update = TaskUpdateTool { list: list.clone() };
        let r = update.execute(json!({"taskId": "1", "status": "in_progress"}), &cancel()).await.unwrap();
        assert!(!r.is_error);
        assert!(r.content.contains("status"));

        let guard = list.lock().unwrap();
        assert_eq!(guard.get("1").unwrap().status, TodoStatus::InProgress);
    }

    #[tokio::test]
    async fn update_owner_and_subject() {
        let list = make_list();
        let create = TaskCreateTool { list: list.clone() };
        create.execute(json!({"subject": "Old", "description": ""}), &cancel()).await.unwrap();

        let update = TaskUpdateTool { list: list.clone() };
        update
            .execute(json!({"taskId": "1", "owner": "alice", "subject": "New"}), &cancel())
            .await
            .unwrap();

        let guard = list.lock().unwrap();
        let t = guard.get("1").unwrap();
        assert_eq!(t.owner.as_deref(), Some("alice"));
        assert_eq!(t.subject, "New");
    }

    #[tokio::test]
    async fn unknown_task_id_returns_error() {
        let list = make_list();
        let update = TaskUpdateTool { list };
        let r = update.execute(json!({"taskId": "99", "status": "completed"}), &cancel()).await.unwrap();
        assert!(r.is_error);
    }

    #[tokio::test]
    async fn missing_task_id_returns_error() {
        let list = make_list();
        let update = TaskUpdateTool { list };
        let r = update.execute(json!({"status": "completed"}), &cancel()).await.unwrap();
        assert!(r.is_error);
    }

    #[tokio::test]
    async fn add_blocked_by_dependency() {
        let list = make_list();
        let create = TaskCreateTool { list: list.clone() };
        create.execute(json!({"subject": "T1", "description": ""}), &cancel()).await.unwrap();
        create.execute(json!({"subject": "T2", "description": ""}), &cancel()).await.unwrap();

        let update = TaskUpdateTool { list: list.clone() };
        update
            .execute(json!({"taskId": "2", "addBlockedBy": ["1"]}), &cancel())
            .await
            .unwrap();

        let guard = list.lock().unwrap();
        assert!(guard.get("2").unwrap().blocked_by.contains(&"1".to_string()));
    }
}
