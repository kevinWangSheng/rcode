//! TaskOutput tool — get the output of a background task.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cc_agents::TaskRegistry;
use cc_core::CcResult;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolInputSchema, ToolResult};

pub struct TaskOutputTool {
    pub registry: Arc<Mutex<TaskRegistry>>,
}

#[async_trait]
impl Tool for TaskOutputTool {
    fn name(&self) -> &str {
        "TaskOutput"
    }

    fn description(&self) -> &str {
        "Get the current status and output of a background task. For completed tasks, \
         returns the task output. For running tasks, returns the current status. \
         Poll this periodically to check on long-running background tasks."
    }

    fn input_schema(&self) -> ToolInputSchema {
        serde_json::from_value(json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "The ID of the background task to query"
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

        let result = {
            let mut reg = self.registry.lock().unwrap();
            // Poll to update completed task states before querying
            reg.poll_completed();
            reg.status(&task_id).map(|state| {
                json!({
                    "task_id": task_id_str,
                    "status": format!("{:?}", state.status).to_lowercase(),
                    "description": state.description,
                    "kind": format!("{:?}", state.kind),
                    "output_tail": state.output_tail,
                })
            })
        };

        match result {
            None => Ok(ToolResult::error(format!("task {task_id_str} not found"))),
            Some(info) => Ok(ToolResult::ok(info.to_string())),
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
    async fn returns_status_for_running_task() {
        let registry = make_registry();
        let task_id = {
            let mut reg = registry.lock().unwrap();
            let c = CancellationToken::new();
            reg.spawn(TaskKind::LocalBash, "slow task".into(), c, async {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                Ok(TaskOutput {
                    summary: "".into(),
                    content: "".into(),
                })
            })
            .unwrap()
        };

        let tool = TaskOutputTool { registry };
        let r = tool
            .execute(json!({"task_id": task_id.0}), &cancel())
            .await
            .unwrap();
        assert!(!r.is_error);
        let v: Value = serde_json::from_str(&r.content).unwrap();
        assert_eq!(v["status"], "running");
    }

    #[tokio::test]
    async fn unknown_task_returns_error() {
        let registry = make_registry();
        let tool = TaskOutputTool { registry };
        let r = tool
            .execute(json!({"task_id": "unknown"}), &cancel())
            .await
            .unwrap();
        assert!(r.is_error);
    }

    #[tokio::test]
    async fn missing_task_id_returns_error() {
        let registry = make_registry();
        let tool = TaskOutputTool { registry };
        let r = tool.execute(json!({}), &cancel()).await.unwrap();
        assert!(r.is_error);
    }
}
