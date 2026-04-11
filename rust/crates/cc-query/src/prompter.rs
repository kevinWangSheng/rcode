//! Headless stdin-based permission prompter.
//!
//! The TUI provides its own prompter via the `AppEvent::PermissionRequest`
//! channel; this module is for headless / CLI mode only.

use cc_core::{CcResult, PermissionPrompter, PromptDecision};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// Default prompter that reads y/N/a from stdin and writes the prompt to stderr.
/// Used when running the binary in headless mode.
pub struct StdinPrompter {
    pub non_interactive: bool,
}

impl StdinPrompter {
    pub fn new(non_interactive: bool) -> Self {
        Self { non_interactive }
    }
}

#[async_trait::async_trait]
impl PermissionPrompter for StdinPrompter {
    async fn prompt(
        &self,
        tool_name: &str,
        input: &Value,
        _cancel: &CancellationToken,
    ) -> CcResult<PromptDecision> {
        Ok(crate::permission_prompt::stdin_prompt(tool_name, input, self.non_interactive).await)
    }
}
