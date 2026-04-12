//! Headless stdin-based permission prompter.
//!
//! The TUI provides its own prompter via the `AppEvent::PermissionRequest`
//! channel; this module is for headless / CLI mode only.

use cc_core::{CcError, CcResult, PermissionPrompter, PromptDecision};
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

    async fn ask_question(
        &self,
        question: &str,
        options: &[String],
        cancel: &CancellationToken,
    ) -> CcResult<String> {
        if self.non_interactive {
            return Ok("User is not available to answer questions in non-interactive mode.".to_string());
        }

        // Print question and options to stderr
        eprintln!("\n{question}");
        for (i, opt) in options.iter().enumerate() {
            eprintln!("  {}. {opt}", i + 1);
        }
        if !options.is_empty() {
            eprintln!("Enter a number or type your answer:");
        }
        eprint!("> ");

        // Read answer from stdin (blocking, so use spawn_blocking)
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(CcError::Cancelled),
            result = tokio::task::spawn_blocking(|| {
                let mut buf = String::new();
                std::io::stdin().read_line(&mut buf)?;
                Ok::<_, std::io::Error>(buf.trim().to_string())
            }) => {
                result
                    .map_err(|e| CcError::Other(format!("stdin error: {e}")))?
                    .map_err(|e| CcError::Other(format!("stdin error: {e}")))
            }
        }
    }
}
