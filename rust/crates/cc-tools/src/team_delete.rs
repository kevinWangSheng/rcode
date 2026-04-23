//! TeamDelete — remove a named teammate agent.
//!
//! Unregisters a teammate from the TeammateDirectory. If the teammate's
//! task is still running, its inbox will be closed causing it to exit
//! on the next iteration.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cc_agents::TeammateDirectory;
use cc_core::CcResult;
use serde_json::{json, Value};

use crate::{Tool, ToolContext, ToolInputSchema, ToolResult};

pub struct TeamDeleteTool {
    pub directory: Arc<Mutex<TeammateDirectory>>,
}

#[async_trait]
impl Tool for TeamDeleteTool {
    fn name(&self) -> &str {
        "TeamDelete"
    }

    fn description(&self) -> &str {
        "Remove a named teammate agent from the team. Closing the teammate's inbox \
         will cause it to exit after finishing its current turn. \
         Use this when a teammate's work is complete."
    }

    fn input_schema(&self) -> ToolInputSchema {
        serde_json::from_value(json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Name of the teammate to remove."
                }
            },
            "required": ["name"]
        }))
        .unwrap()
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> CcResult<ToolResult> {
        let name = match input.get("name").and_then(Value::as_str) {
            Some(n) => n.to_string(),
            None => return Ok(ToolResult::error("missing required field: name")),
        };

        let removed = {
            let mut dir = self.directory.lock().unwrap();
            dir.remove(&name).is_some()
        };

        if removed {
            let result = json!({
                "name": name,
                "status": "removed",
                "message": format!("Teammate '{name}' removed from the team.")
            });
            Ok(ToolResult::ok(result.to_string()))
        } else {
            Ok(ToolResult::error(format!("teammate '{name}' not found")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cc_core::task::TaskId;
    use tokio_util::sync::CancellationToken;

    fn make_tool() -> TeamDeleteTool {
        let dir = Arc::new(Mutex::new(TeammateDirectory::new()));
        {
            let (tx, _rx) = tokio::sync::mpsc::channel(1);
            let id = TaskId::new();
            dir.lock().unwrap().register("worker".into(), id, tx);
        }
        TeamDeleteTool { directory: dir }
    }

    #[tokio::test]
    async fn removes_existing_teammate() {
        let tool = make_tool();
        let r = tool
            .execute(
                json!({"name": "worker"}),
                &ToolContext::for_test_bare(CancellationToken::new()),
            )
            .await
            .unwrap();
        assert!(!r.is_error);
        assert!(r.content.contains("removed"));
    }

    #[tokio::test]
    async fn missing_teammate_returns_error() {
        let tool = make_tool();
        let r = tool
            .execute(
                json!({"name": "nobody"}),
                &ToolContext::for_test_bare(CancellationToken::new()),
            )
            .await
            .unwrap();
        assert!(r.is_error);
    }
}
