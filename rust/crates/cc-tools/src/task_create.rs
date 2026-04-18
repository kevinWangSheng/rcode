//! TaskCreate tool — create a new entry in the todo task list.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cc_core::CcResult;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::todo::TodoList;
use crate::{Tool, ToolInputSchema, ToolResult};

pub struct TaskCreateTool {
    pub list: Arc<Mutex<TodoList>>,
}

#[async_trait]
impl Tool for TaskCreateTool {
    fn name(&self) -> &str {
        "TaskCreate"
    }

    fn description(&self) -> &str {
        "Create a new task in the task list. Use proactively for complex multi-step tasks \
         (3+ steps), plan mode, when the user provides multiple tasks, or when you start working \
         and need to track progress. All tasks are created with status `pending`."
    }

    fn input_schema(&self) -> ToolInputSchema {
        serde_json::from_value(json!({
            "type": "object",
            "properties": {
                "subject": {
                    "type": "string",
                    "description": "A brief title for the task"
                },
                "description": {
                    "type": "string",
                    "description": "What needs to be done"
                },
                "activeForm": {
                    "type": "string",
                    "description": "Present continuous form shown in spinner when in_progress (e.g., \"Running tests\")"
                },
                "metadata": {
                    "type": "object",
                    "description": "Arbitrary metadata to attach to the task",
                    "additionalProperties": true
                }
            },
            "required": ["subject", "description"]
        }))
        .unwrap()
    }

    async fn execute(&self, input: Value, _cancel: &CancellationToken) -> CcResult<ToolResult> {
        let subject = match input.get("subject").and_then(Value::as_str) {
            Some(s) => s.to_string(),
            None => return Ok(ToolResult::error("missing required field: subject")),
        };
        let description = match input.get("description").and_then(Value::as_str) {
            Some(s) => s.to_string(),
            None => return Ok(ToolResult::error("missing required field: description")),
        };
        let active_form = input
            .get("activeForm")
            .and_then(Value::as_str)
            .map(str::to_string);

        let metadata = input
            .get("metadata")
            .and_then(Value::as_object)
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect());

        let id = {
            let mut list = self.list.lock().unwrap();
            list.create(subject.clone(), description, active_form, metadata)
        };

        let content = json!({
            "task": {
                "id": id,
                "subject": subject
            }
        })
        .to_string();

        Ok(ToolResult::ok(content))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tool() -> TaskCreateTool {
        TaskCreateTool {
            list: Arc::new(Mutex::new(TodoList::new())),
        }
    }

    fn cancel() -> CancellationToken {
        CancellationToken::new()
    }

    #[tokio::test]
    async fn creates_task_and_returns_id() {
        let tool = make_tool();
        let result = tool
            .execute(
                json!({"subject": "Fix bug", "description": "Fix the login bug"}),
                &cancel(),
            )
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("\"id\":\"1\""));
        assert!(result.content.contains("Fix bug"));
    }

    #[tokio::test]
    async fn missing_subject_returns_error() {
        let tool = make_tool();
        let result = tool
            .execute(json!({"description": "no subject"}), &cancel())
            .await
            .unwrap();
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn sequential_ids() {
        let list = Arc::new(Mutex::new(TodoList::new()));
        let tool = TaskCreateTool { list: list.clone() };
        let r1 = tool
            .execute(json!({"subject": "T1", "description": ""}), &cancel())
            .await
            .unwrap();
        let r2 = tool
            .execute(json!({"subject": "T2", "description": ""}), &cancel())
            .await
            .unwrap();
        assert!(r1.content.contains("\"id\":\"1\""));
        assert!(r2.content.contains("\"id\":\"2\""));
    }

    #[tokio::test]
    async fn task_is_pending_by_default() {
        let list = Arc::new(Mutex::new(TodoList::new()));
        let tool = TaskCreateTool { list: list.clone() };
        tool.execute(json!({"subject": "T", "description": ""}), &cancel())
            .await
            .unwrap();
        let guard = list.lock().unwrap();
        let task = guard.get("1").unwrap();
        assert_eq!(task.status, crate::todo::TodoStatus::Pending);
    }
}
