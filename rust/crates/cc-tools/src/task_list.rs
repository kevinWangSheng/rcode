//! TaskList tool — list all tasks in the todo list.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cc_core::CcResult;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::todo::TodoList;
use crate::{Tool, ToolInputSchema, ToolResult};

pub struct TaskListTool {
    pub list: Arc<Mutex<TodoList>>,
}

#[async_trait]
impl Tool for TaskListTool {
    fn name(&self) -> &str {
        "TaskList"
    }

    fn description(&self) -> &str {
        "List all tasks in the task list. Use to check overall progress, find available work \
         (status: pending, no owner, not blocked), or find your next task after completing one. \
         Prefer working on tasks in ID order (lowest first)."
    }

    fn input_schema(&self) -> ToolInputSchema {
        serde_json::from_value(json!({
            "type": "object",
            "properties": {}
        }))
        .unwrap()
    }

    async fn execute(&self, _input: Value, _cancel: &CancellationToken) -> CcResult<ToolResult> {
        let guard = self.list.lock().unwrap();
        let tasks = guard.list();

        if tasks.is_empty() {
            return Ok(ToolResult::ok("No tasks found".to_string()));
        }

        let summary: Vec<Value> = tasks
            .iter()
            .map(|t| {
                let mut obj = json!({
                    "id": t.id,
                    "subject": t.subject,
                    "status": t.status.to_string(),
                });
                if let Some(owner) = &t.owner {
                    obj["owner"] = Value::String(owner.clone());
                }
                if !t.blocked_by.is_empty() {
                    obj["blockedBy"] = json!(t.blocked_by);
                }
                obj
            })
            .collect();

        Ok(ToolResult::ok(serde_json::to_string(&summary).unwrap()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task_create::TaskCreateTool;
    use crate::task_update::TaskUpdateTool;

    fn cancel() -> CancellationToken {
        CancellationToken::new()
    }

    #[tokio::test]
    async fn empty_list_returns_no_tasks() {
        let list = Arc::new(Mutex::new(TodoList::new()));
        let tool = TaskListTool { list };
        let r = tool.execute(json!({}), &cancel()).await.unwrap();
        assert!(!r.is_error);
        assert!(r.content.contains("No tasks"));
    }

    #[tokio::test]
    async fn lists_all_non_deleted_tasks() {
        let list = Arc::new(Mutex::new(TodoList::new()));
        let create = TaskCreateTool { list: list.clone() };
        create
            .execute(json!({"subject": "T1", "description": ""}), &cancel())
            .await
            .unwrap();
        create
            .execute(json!({"subject": "T2", "description": ""}), &cancel())
            .await
            .unwrap();
        let update = TaskUpdateTool { list: list.clone() };
        update
            .execute(json!({"taskId": "1", "status": "deleted"}), &cancel())
            .await
            .unwrap();

        let list_tool = TaskListTool { list };
        let r = list_tool.execute(json!({}), &cancel()).await.unwrap();
        assert!(!r.is_error);
        let arr: Vec<Value> = serde_json::from_str(&r.content).unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["subject"], "T2");
    }

    #[tokio::test]
    async fn shows_owner_and_blocked_by_when_set() {
        let list = Arc::new(Mutex::new(TodoList::new()));
        let create = TaskCreateTool { list: list.clone() };
        create
            .execute(json!({"subject": "T1", "description": ""}), &cancel())
            .await
            .unwrap();
        create
            .execute(json!({"subject": "T2", "description": ""}), &cancel())
            .await
            .unwrap();
        let update = TaskUpdateTool { list: list.clone() };
        update
            .execute(
                json!({"taskId": "2", "owner": "bob", "addBlockedBy": ["1"]}),
                &cancel(),
            )
            .await
            .unwrap();

        let list_tool = TaskListTool { list };
        let r = list_tool.execute(json!({}), &cancel()).await.unwrap();
        let arr: Vec<Value> = serde_json::from_str(&r.content).unwrap();
        let t2 = arr.iter().find(|t| t["id"] == "2").unwrap();
        assert_eq!(t2["owner"], "bob");
        assert_eq!(t2["blockedBy"], json!(["1"]));
    }

    #[tokio::test]
    async fn status_shown_correctly() {
        let list = Arc::new(Mutex::new(TodoList::new()));
        let create = TaskCreateTool { list: list.clone() };
        create
            .execute(json!({"subject": "T", "description": ""}), &cancel())
            .await
            .unwrap();
        let update = TaskUpdateTool { list: list.clone() };
        update
            .execute(json!({"taskId": "1", "status": "in_progress"}), &cancel())
            .await
            .unwrap();

        let list_tool = TaskListTool { list };
        let r = list_tool.execute(json!({}), &cancel()).await.unwrap();
        let arr: Vec<Value> = serde_json::from_str(&r.content).unwrap();
        assert_eq!(arr[0]["status"], "in_progress");
    }
}
