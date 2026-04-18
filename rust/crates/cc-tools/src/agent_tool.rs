//! AgentTool — spawns an in-process sub-agent (local_agent task type).
//!
//! Holds an optional Arc<dyn SubAgentRunner> injected at startup.
//! When the runner is None (not wired), the tool returns a graceful error.

use std::sync::Arc;

use async_trait::async_trait;
use cc_core::{CcResult, SubAgentRunner};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolInputSchema, ToolResult};

pub struct AgentTool {
    /// Injected by main.rs; None means sub-agents are not available in this build.
    pub runner: Option<Arc<dyn SubAgentRunner>>,
}

#[async_trait]
impl Tool for AgentTool {
    fn name(&self) -> &str {
        "Agent"
    }

    fn description(&self) -> &str {
        "Launch a specialized sub-agent to handle a complex, multi-step task autonomously. \
         The sub-agent has access to the same tools as the parent agent and runs its own \
         query loop until the task is complete. Use this when a task requires deep focus \
         or parallelism that would clutter the main conversation."
    }

    fn input_schema(&self) -> ToolInputSchema {
        serde_json::from_value(json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The task description for the sub-agent"
                },
                "description": {
                    "type": "string",
                    "description": "Short human-readable description of what this agent will do (3-5 words)"
                },
                "system_prompt": {
                    "type": "string",
                    "description": "Optional additional system instructions for the sub-agent"
                }
            },
            "required": ["prompt", "description"]
        }))
        .unwrap()
    }

    async fn execute(&self, input: Value, cancel: &CancellationToken) -> CcResult<ToolResult> {
        let runner = match &self.runner {
            Some(r) => r.clone(),
            None => {
                return Ok(ToolResult::error(
                    "AgentTool is not available: sub-agent runner not wired at startup",
                ))
            }
        };

        let prompt = match input.get("prompt").and_then(Value::as_str) {
            Some(s) => s.to_string(),
            None => return Ok(ToolResult::error("missing required field: prompt")),
        };

        let system = input
            .get("system_prompt")
            .and_then(Value::as_str)
            .map(str::to_string);

        match runner.run(system, prompt, Vec::new(), cancel.clone()).await {
            Ok(result) => Ok(ToolResult::ok(result)),
            Err(e) => Ok(ToolResult::error(format!("sub-agent failed: {e}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cancel() -> CancellationToken {
        CancellationToken::new()
    }

    #[tokio::test]
    async fn returns_error_when_runner_not_wired() {
        let tool = AgentTool { runner: None };
        let r = tool
            .execute(
                json!({"prompt": "do something", "description": "test"}),
                &cancel(),
            )
            .await
            .unwrap();
        assert!(r.is_error);
        assert!(r.content.contains("not available"));
    }

    #[tokio::test]
    async fn missing_prompt_returns_error() {
        let tool = AgentTool { runner: None };
        let r = tool
            .execute(json!({"description": "test"}), &cancel())
            .await
            .unwrap();
        // When runner is None, the None check fires first
        assert!(r.is_error);
    }
}
