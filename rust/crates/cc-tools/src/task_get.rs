//! TaskGet tool — retrieve a single task by ID with full details.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cc_core::CcResult;
use serde_json::{json, Value};

use crate::todo::TodoList;
use crate::{Tool, ToolContext, ToolInputSchema, ToolResult};

pub struct TaskGetTool {
    pub list: Arc<Mutex<TodoList>>,
}

#[async_trait]
impl Tool for TaskGetTool {
    fn name(&self) -> &str {
        "TaskGet"
    }

    fn description(&self) -> &str {
        "Retrieve a task by its ID from the task list. Returns full details including \
         description, status, blocks, and blockedBy. Use before updating a task to get \
         its current state and avoid operating on stale data."
    }

    fn input_schema(&self) -> ToolInputSchema {
        serde_json::from_value(json!({
            "type": "object",
            "properties": {
                "taskId": {
                    "type": "string",
                    "description": "The ID of the task to retrieve"
                }
            },
            "required": ["taskId"]
        }))
        .unwrap()
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> CcResult<ToolResult> {
        let task_id = match input.get("taskId").and_then(Value::as_str) {
            Some(s) => s,
            None => return Ok(ToolResult::error("missing required field: taskId")),
        };

        let guard = self.list.lock().unwrap();
        match guard.get(task_id) {
            None => Ok(ToolResult::ok(json!({"task": null}).to_string())),
            Some(task) => {
                let mut obj = json!({
                    "id": task.id,
                    "subject": task.subject,
                    "description": task.description,
                    "status": task.status.to_string(),
                    "blocks": task.blocks,
                    "blockedBy": task.blocked_by,
                });
                if let Some(owner) = &task.owner {
                    obj["owner"] = Value::String(owner.clone());
                }
                if let Some(af) = &task.active_form {
                    obj["activeForm"] = Value::String(af.clone());
                }
                if !task.metadata.is_empty() {
                    obj["metadata"] = json!(task.metadata);
                }
                Ok(ToolResult::ok(json!({"task": obj}).to_string()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task_create::TaskCreateTool;
    use tokio_util::sync::CancellationToken;

    fn ctx() -> ToolContext {
        ToolContext::for_test_bare(CancellationToken::new())
    }

    #[tokio::test]
    async fn returns_null_for_unknown_id() {
        let list = Arc::new(Mutex::new(TodoList::new()));
        let tool = TaskGetTool { list };
        let r = tool
            .execute(json!({"taskId": "999"}), &ctx())
            .await
            .unwrap();
        assert!(!r.is_error);
        assert!(r.content.contains("null"));
    }

    #[tokio::test]
    async fn returns_full_task_details() {
        let list = Arc::new(Mutex::new(TodoList::new()));
        let create = TaskCreateTool { list: list.clone() };
        create
            .execute(
                json!({"subject": "Fix it", "description": "Full description", "activeForm": "Fixing"}),
                &ctx(),
            )
            .await
            .unwrap();

        let tool = TaskGetTool { list };
        let r = tool.execute(json!({"taskId": "1"}), &ctx()).await.unwrap();
        assert!(!r.is_error);
        let v: Value = serde_json::from_str(&r.content).unwrap();
        let task = &v["task"];
        assert_eq!(task["id"], "1");
        assert_eq!(task["subject"], "Fix it");
        assert_eq!(task["description"], "Full description");
        assert_eq!(task["activeForm"], "Fixing");
        assert_eq!(task["status"], "pending");
    }

    #[tokio::test]
    async fn missing_task_id_returns_error() {
        let list = Arc::new(Mutex::new(TodoList::new()));
        let tool = TaskGetTool { list };
        let r = tool.execute(json!({}), &ctx()).await.unwrap();
        assert!(r.is_error);
    }

    #[tokio::test]
    async fn blocks_and_blocked_by_present_in_output() {
        let list = Arc::new(Mutex::new(TodoList::new()));
        let create = TaskCreateTool { list: list.clone() };
        create
            .execute(json!({"subject": "T", "description": ""}), &ctx())
            .await
            .unwrap();
        let tool = TaskGetTool { list };
        let r = tool.execute(json!({"taskId": "1"}), &ctx()).await.unwrap();
        let v: Value = serde_json::from_str(&r.content).unwrap();
        assert!(v["task"]["blocks"].is_array());
        assert!(v["task"]["blockedBy"].is_array());
    }
}
