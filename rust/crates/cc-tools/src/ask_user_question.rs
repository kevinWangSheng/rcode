//! AskUserQuestion — prompt the user for input during a tool turn.
//!
//! Allows the assistant to ask the user a question and receive a text response.
//! In non-interactive / headless mode the tool returns a graceful message
//! explaining the user cannot be reached; in interactive / TUI mode the
//! PermissionPrompter implementation handles routing to the UI.

use std::sync::Arc;

use async_trait::async_trait;
use cc_core::{CcResult, PermissionPrompter};
use serde_json::{json, Value};

use crate::{Tool, ToolContext, ToolInputSchema, ToolResult};

pub struct AskUserQuestionTool {
    /// Wired at startup by main.rs; used to ask the user a question.
    pub prompter: Arc<dyn PermissionPrompter>,
}

#[async_trait]
impl Tool for AskUserQuestionTool {
    fn name(&self) -> &str {
        "AskUserQuestion"
    }

    fn description(&self) -> &str {
        "Ask the user a question and wait for their response. Use this when you need \
         clarification, a decision, or input that you cannot infer from context. \
         Provide clear options when the answer space is bounded. \
         Do not use this for yes/no permission questions — those go through the normal \
         permission system."
    }

    fn input_schema(&self) -> ToolInputSchema {
        serde_json::from_value(json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "The question to ask the user. Should be clear and specific."
                },
                "options": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Optional list of suggested answers. When provided, displayed as a numbered list."
                }
            },
            "required": ["question"]
        }))
        .unwrap()
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> CcResult<ToolResult> {
        let question = match input.get("question").and_then(Value::as_str) {
            Some(q) => q.to_string(),
            None => return Ok(ToolResult::error("missing required field: question")),
        };

        let options: Vec<String> = input
            .get("options")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();

        let answer = self
            .prompter
            .ask_question(&question, &options, &ctx.cancel)
            .await?;

        let result = json!({
            "question": question,
            "answer": answer,
        });
        Ok(ToolResult::ok(result.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cc_core::{CcResult, PromptDecision};
    use tokio_util::sync::CancellationToken;

    struct AlwaysAnswer(String);

    #[async_trait::async_trait]
    impl PermissionPrompter for AlwaysAnswer {
        async fn prompt(
            &self,
            _: &str,
            _: &Value,
            _: &CancellationToken,
        ) -> CcResult<PromptDecision> {
            Ok(PromptDecision::Allow)
        }

        async fn ask_question(
            &self,
            _: &str,
            _: &[String],
            _: &CancellationToken,
        ) -> CcResult<String> {
            Ok(self.0.clone())
        }
    }

    fn tool(answer: &str) -> AskUserQuestionTool {
        AskUserQuestionTool {
            prompter: Arc::new(AlwaysAnswer(answer.to_string())),
        }
    }

    fn ctx() -> ToolContext {
        ToolContext::for_test_bare(CancellationToken::new())
    }

    #[tokio::test]
    async fn returns_answer() {
        let t = tool("TypeScript");
        let r = t
            .execute(
                json!({"question": "Which language?", "options": ["Rust", "TypeScript"]}),
                &ctx(),
            )
            .await
            .unwrap();
        assert!(!r.is_error);
        assert!(r.content.contains("TypeScript"));
    }

    #[tokio::test]
    async fn missing_question_returns_error() {
        let t = tool("yes");
        let r = t.execute(json!({}), &ctx()).await.unwrap();
        assert!(r.is_error);
    }
}
