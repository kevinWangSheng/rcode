//! TaskStop tool — stop a running background task in the TaskRegistry.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cc_agents::TaskRegistry;
use cc_core::CcResult;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolInputSchema, ToolResult};

pub struct TaskStopTool {
    pub registry: Arc<Mutex<TaskRegistry>>,
}

#[async_trait]
impl Tool for TaskStopTool {
    fn name(&self) -> &str {
        "TaskStop"
    }

    fn description(&self) -> &str {
        "Stop a running background task (local_bash, remote_agent, etc.) by cancelling it. \
         The task is given a Cancelled status. Use TaskList to find the task_id of the \
         running task you want to stop."
    }

    fn input_schema(&self) -> ToolInputSchema {
        serde_json::from_value(json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "The ID of the background task to stop"
                }
            },
            "required": ["task_id"]
        }))
        .unwrap()
    }

    async fn execute(&self, input: Value, _cancel: &CancellationToken) -> CcResult<ToolResult> {
        let task_id_str = match input.get("task_id").and_then(Value::as_str) {
            Some(s) => s.to_string(),
            None => return Ok(ToolResult::error("missing required field: task_id")),
        };

        let task_id = cc_core::task::TaskId(task_id_str.clone());

        let (found, status) = {
            let mut reg = self.registry.lock().unwrap();
            let exists = reg.status(&task_id).is_some();
            if exists {
                reg.cancel(&task_id);
                let new_status = reg
                    .status(&task_id)
                    .map(|s| format!("{:?}", s.status))
                    .unwrap_or_else(|| "unknown".to_string());
                (true, new_status)
            } else {
                (false, String::new())
            }
        };

        if found {
            Ok(ToolResult::ok(
                json!({
                    "task_id": task_id_str,
                    "status": status,
                    "message": format!("Task {task_id_str} stopped")
                })
                .to_string(),
            ))
        } else {
            Ok(ToolResult::error(format!("task {task_id_str} not found")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cc_agents::TaskOutput;
    use cc_core::task::TaskKind;

    fn make_registry() -> Arc<Mutex<TaskRegistry>> {
        Arc::new(Mutex::new(TaskRegistry::new(10)))
    }

    fn cancel() -> CancellationToken {
        CancellationToken::new()
    }

    #[tokio::test]
    async fn stops_running_task() {
        let registry = make_registry();
        let task_id = {
            let mut reg = registry.lock().unwrap();
            let c = CancellationToken::new();
            reg.spawn(TaskKind::LocalBash, "long task".into(), c, async {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                Ok(TaskOutput {
                    summary: "".into(),
                    content: "".into(),
                })
            })
            .unwrap()
        };

        let tool = TaskStopTool {
            registry: registry.clone(),
        };
        let r = tool
            .execute(json!({"task_id": task_id.0}), &cancel())
            .await
            .unwrap();
        assert!(!r.is_error, "unexpected error: {}", r.content);
        assert!(r.content.contains("stopped"));
    }

    #[tokio::test]
    async fn unknown_task_id_returns_error() {
        let registry = make_registry();
        let tool = TaskStopTool { registry };
        let r = tool
            .execute(json!({"task_id": "nonexistent-id"}), &cancel())
            .await
            .unwrap();
        assert!(r.is_error);
    }

    #[tokio::test]
    async fn missing_task_id_returns_error() {
        let registry = make_registry();
        let tool = TaskStopTool { registry };
        let r = tool.execute(json!({}), &cancel()).await.unwrap();
        assert!(r.is_error);
    }
}
