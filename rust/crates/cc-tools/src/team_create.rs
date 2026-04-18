//! TeamCreate — spawn a named in-process teammate agent.
//!
//! Creates a sub-agent with a name and registers it in the TeammateDirectory
//! so that SendMessage can route messages to it. The agent starts with the
//! given prompt and then processes further instructions from its inbox.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cc_agents::TeammateDirectory;
use cc_core::task::TaskId;
use cc_core::{CcResult, SubAgentRunner};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolInputSchema, ToolResult};

pub struct TeamCreateTool {
    /// Injected at startup via the same pattern as AgentTool.
    pub runner: Option<Arc<dyn SubAgentRunner>>,
    pub directory: Arc<Mutex<TeammateDirectory>>,
}

#[async_trait]
impl Tool for TeamCreateTool {
    fn name(&self) -> &str {
        "TeamCreate"
    }

    fn description(&self) -> &str {
        "Spawn a named in-process teammate agent. The teammate runs with the given \
         initial prompt and then listens for further messages via SendMessage. \
         Each teammate has a unique name used to route messages to it."
    }

    fn input_schema(&self) -> ToolInputSchema {
        serde_json::from_value(json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Unique name for this teammate (e.g., 'researcher', 'reviewer')."
                },
                "prompt": {
                    "type": "string",
                    "description": "Initial task/role description for the teammate."
                },
                "system_prompt": {
                    "type": "string",
                    "description": "Optional additional system instructions for this teammate."
                }
            },
            "required": ["name", "prompt"]
        }))
        .unwrap()
    }

    async fn execute(&self, input: Value, cancel: &CancellationToken) -> CcResult<ToolResult> {
        let runner = match &self.runner {
            Some(r) => r.clone(),
            None => {
                return Ok(ToolResult::error(
                    "TeamCreate is not available: sub-agent runner not wired at startup",
                ))
            }
        };

        let name = match input.get("name").and_then(Value::as_str) {
            Some(n) => n.to_string(),
            None => return Ok(ToolResult::error("missing required field: name")),
        };
        let prompt = match input.get("prompt").and_then(Value::as_str) {
            Some(p) => p.to_string(),
            None => return Ok(ToolResult::error("missing required field: prompt")),
        };
        let system = input
            .get("system_prompt")
            .and_then(Value::as_str)
            .map(str::to_string);

        // Check if name is already registered
        {
            let dir = self.directory.lock().unwrap();
            if dir.contains(&name) {
                return Ok(ToolResult::error(format!(
                    "teammate '{name}' already exists; use a different name or TeamDelete first"
                )));
            }
        }

        // Create mailbox: parent sends instructions, teammate receives
        let (inbox_tx, mut inbox_rx) = tokio::sync::mpsc::channel::<String>(64);

        // Register the teammate BEFORE spawning the task
        let task_id = TaskId::new();
        {
            let mut dir = self.directory.lock().unwrap();
            dir.register(name.clone(), task_id, inbox_tx);
        }

        // Spawn the teammate as a background task
        let name_clone = name.clone();
        let directory_clone = self.directory.clone();
        let cancel_clone = cancel.clone();

        tokio::spawn(async move {
            // Run initial turn
            let _ = runner
                .run(system, prompt, Vec::new(), cancel_clone.clone())
                .await;

            // Then process inbox messages until cancelled or inbox closed
            loop {
                tokio::select! {
                    biased;
                    _ = cancel_clone.cancelled() => break,
                    msg = inbox_rx.recv() => {
                        match msg {
                            None => break, // inbox closed
                            Some(message) => {
                                let _ = runner
                                    .run(None, message, Vec::new(), cancel_clone.clone())
                                    .await;
                            }
                        }
                    }
                }
            }

            // Unregister on exit
            let mut dir = directory_clone.lock().unwrap();
            dir.remove(&name_clone);
        });

        let result = json!({
            "name": name,
            "status": "spawned",
            "message": format!("Teammate '{name}' spawned. Use SendMessage to communicate.")
        });
        Ok(ToolResult::ok(result.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn returns_error_when_runner_not_wired() {
        let tool = TeamCreateTool {
            runner: None,
            directory: Arc::new(Mutex::new(TeammateDirectory::new())),
        };
        let r = tool
            .execute(
                json!({"name": "test", "prompt": "do something"}),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(r.is_error);
    }
}
