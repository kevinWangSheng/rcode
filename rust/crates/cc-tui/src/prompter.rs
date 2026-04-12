//! Channel-based permission prompter for the TUI.
//!
//! The query engine runs in a tokio task; when it needs to prompt the user for
//! permission to run a tool, it calls this prompter. The prompter forwards the
//! request to the TUI main loop over an mpsc channel and waits on a oneshot for
//! the user's reply. This decouples the engine from the renderer entirely — the
//! engine doesn't know whether it's talking to a terminal, a unit test, or a
//! mocked GUI.

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use cc_core::{AppEvent, CcResult, PermissionPrompter, PromptDecision};

/// Monotonic counter for permission request IDs.
static NEXT_PERM_ID: AtomicU64 = AtomicU64::new(1);

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
        let id = NEXT_PERM_ID.fetch_add(1, Ordering::Relaxed);
        let (response_tx, response_rx) = oneshot::channel();
        let req = AppEvent::PermissionRequest {
            id,
            tool_name: tool_name.to_string(),
            tool_input: input.clone(),
            response_tx,
        };
        if self.tx.send(req).await.is_err() {
            // UI is gone — fail closed.
            return Ok(PromptDecision::Deny);
        }
        // If the UI drops the reply channel or cancel fires, default to Deny.
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Ok(PromptDecision::Deny),
            result = response_rx => {
                let decision: PromptDecision = result.unwrap_or(PromptDecision::Deny);
                Ok(decision)
            }
        }
    }
}
