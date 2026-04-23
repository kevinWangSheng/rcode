//! SendMessage — send a message to another agent (teammate).
//!
//! Routes a message to a named teammate registered in the TeammateDirectory.
//! The teammate's main loop will deliver the message as a user turn.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cc_agents::TeammateDirectory;
use cc_core::CcResult;
use serde_json::{json, Value};

use crate::{Tool, ToolContext, ToolInputSchema, ToolResult};

pub struct SendMessageTool {
    pub directory: Arc<Mutex<TeammateDirectory>>,
}

#[async_trait]
impl Tool for SendMessageTool {
    fn name(&self) -> &str {
        "SendMessage"
    }

    fn description(&self) -> &str {
        "Send a message to another agent (teammate) by name. \
         Use this to communicate with sub-agents running in the swarm. \
         Messages are delivered to the teammate's inbox and processed on their next turn. \
         Your plain-text output is NOT visible to other agents — you MUST call this tool \
         to communicate."
    }

    fn input_schema(&self) -> ToolInputSchema {
        serde_json::from_value(json!({
            "type": "object",
            "properties": {
                "to": {
                    "type": "string",
                    "description": "Name of the recipient agent (e.g. 'researcher', 'reviewer'). \
                                    Use '*' to broadcast to all active teammates."
                },
                "message": {
                    "type": "string",
                    "description": "The message to send."
                },
                "summary": {
                    "type": "string",
                    "description": "Optional short description of the message (1-10 words)."
                }
            },
            "required": ["to", "message"]
        }))
        .unwrap()
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> CcResult<ToolResult> {
        let to = match input.get("to").and_then(Value::as_str) {
            Some(t) => t.to_string(),
            None => return Ok(ToolResult::error("missing required field: to")),
        };
        let message = match input.get("message").and_then(Value::as_str) {
            Some(m) => m.to_string(),
            None => return Ok(ToolResult::error("missing required field: message")),
        };

        let dir = self.directory.lock().unwrap();

        if to == "*" {
            // Broadcast
            let names = dir.names();
            if names.is_empty() {
                return Ok(ToolResult::ok("Broadcast: no active teammates to send to."));
            }
            let mut sent_to = Vec::new();
            let mut failed = Vec::new();
            for name in &names {
                if let Some(tx) = dir.get_sender(name) {
                    match tx.try_send(message.clone()) {
                        Ok(()) => sent_to.push(name.clone()),
                        Err(_) => failed.push(name.clone()),
                    }
                }
            }
            let result = json!({
                "sent_to": sent_to,
                "failed": failed,
                "message": format!("Broadcast delivered to {} teammate(s).", sent_to.len())
            });
            return Ok(ToolResult::ok(result.to_string()));
        }

        // Single recipient
        match dir.get_sender(&to) {
            Some(tx) => match tx.try_send(message) {
                Ok(()) => {
                    let result = json!({
                        "recipient": to,
                        "status": "delivered",
                        "message": format!("Message delivered to '{to}'.")
                    });
                    Ok(ToolResult::ok(result.to_string()))
                }
                Err(_) => Ok(ToolResult::error(format!(
                    "failed to deliver message to '{to}': mailbox full or closed"
                ))),
            },
            None => {
                let names = dir.names();
                if names.is_empty() {
                    Ok(ToolResult::error(format!(
                        "teammate '{to}' not found (no active teammates)"
                    )))
                } else {
                    Ok(ToolResult::error(format!(
                        "teammate '{to}' not found; active teammates: {}",
                        names.join(", ")
                    )))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cc_core::task::TaskId;
    use tokio_util::sync::CancellationToken;

    fn make_tool() -> (SendMessageTool, tokio::sync::mpsc::Receiver<String>) {
        let dir = Arc::new(Mutex::new(TeammateDirectory::new()));
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let id = TaskId::new();
        dir.lock().unwrap().register("worker".into(), id, tx);
        (SendMessageTool { directory: dir }, rx)
    }

    fn ctx() -> ToolContext {
        ToolContext::for_test_bare(CancellationToken::new())
    }

    #[tokio::test]
    async fn send_to_known_teammate() {
        let (tool, mut rx) = make_tool();
        let r = tool
            .execute(json!({"to": "worker", "message": "start task 1"}), &ctx())
            .await
            .unwrap();
        assert!(!r.is_error);
        assert_eq!(rx.recv().await.unwrap(), "start task 1");
    }

    #[tokio::test]
    async fn send_to_unknown_returns_error() {
        let (tool, _rx) = make_tool();
        let r = tool
            .execute(json!({"to": "nobody", "message": "hello"}), &ctx())
            .await
            .unwrap();
        assert!(r.is_error);
        assert!(r.content.contains("not found"));
    }

    #[tokio::test]
    async fn missing_fields_return_errors() {
        let (tool, _rx) = make_tool();
        let r = tool.execute(json!({"to": "worker"}), &ctx()).await.unwrap();
        assert!(r.is_error);
    }
}
