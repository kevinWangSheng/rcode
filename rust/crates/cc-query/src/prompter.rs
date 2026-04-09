//! Pluggable permission prompter trait.
//!
//! The query engine asks a `PermissionPrompter` what to do whenever a tool's
//! `PermissionBehavior` is `Ask`. The headless binary uses [`StdinPrompter`]
//! (terminal y/N/a prompt on stderr); the TUI provides its own prompter that
//! displays a modal dialog and forwards the user's choice over a channel.

use async_trait::async_trait;
use serde_json::Value;

use crate::permission_prompt::PromptDecision;

/// A permission prompter answers `Ask` permission checks for the query engine.
///
/// Implementations may be called concurrently from a tokio task driving the
/// engine; they must be `Send + Sync`. Returning [`PromptDecision::Deny`] is
/// always safe and is the convention for non-interactive or aborted prompts.
#[async_trait]
pub trait PermissionPrompter: Send + Sync {
    async fn prompt(&self, tool_name: &str, input: &Value) -> PromptDecision;
}

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

#[async_trait]
impl PermissionPrompter for StdinPrompter {
    async fn prompt(&self, tool_name: &str, input: &Value) -> PromptDecision {
        crate::permission_prompt::stdin_prompt(tool_name, input, self.non_interactive).await
    }
}
