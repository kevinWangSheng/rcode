//! Channel-based permission prompter for the TUI.
//!
//! The query engine runs in a tokio task; when it needs to prompt the user for
//! permission to run a tool, it calls this prompter. The prompter forwards the
//! request to the TUI main loop over an mpsc channel and waits on a oneshot for
//! the user's reply. This decouples the engine from the renderer entirely — the
//! engine doesn't know whether it's talking to a terminal, a unit test, or a
//! mocked GUI.

use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use cc_core::{CcResult, PermissionPrompter, PromptDecision};

use crate::event::AppEvent;

pub struct ChannelPrompter {
    tx: mpsc::Sender<AppEvent>,
}

impl ChannelPrompter {
    pub fn new(tx: mpsc::Sender<AppEvent>) -> Self {
        Self { tx }
    }
}

#[async_trait::async_trait]
impl PermissionPrompter for ChannelPrompter {
    async fn prompt(
        &self,
        tool_name: &str,
        input: &Value,
        cancel: &CancellationToken,
    ) -> CcResult<PromptDecision> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let req = AppEvent::PermissionRequest {
            tool_name: tool_name.to_string(),
            input: input.clone(),
            reply: reply_tx,
        };
        if self.tx.send(req).await.is_err() {
            // UI is gone — fail closed.
            return Ok(PromptDecision::Deny);
        }
        // If the UI drops the reply channel or cancel fires, default to Deny.
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Ok(PromptDecision::Deny),
            result = reply_rx => {
                let decision: PromptDecision = result.unwrap_or(PromptDecision::Deny);
                Ok(decision)
            }
        }
    }
}
