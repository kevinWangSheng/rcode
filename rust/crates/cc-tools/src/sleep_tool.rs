//! SleepTool — wait for a specified duration.
//!
//! Prefer this over `Bash(sleep ...)` — it doesn't hold a shell process and
//! can be interrupted via the CancellationToken.

use crate::{Tool, ToolContext, ToolInputSchema, ToolResult};
use async_trait::async_trait;
use cc_core::{CcError, CcResult};
use serde_json::{json, Value};

pub struct SleepTool;

#[async_trait]
impl Tool for SleepTool {
    fn name(&self) -> &str {
        "Sleep"
    }

    fn description(&self) -> &str {
        "Wait for a specified duration. The user can interrupt the sleep at any time. \
         Use this when you have nothing to do or are waiting for something. \
         Prefer this over Bash(sleep ...) — it doesn't hold a shell process."
    }

    fn input_schema(&self) -> ToolInputSchema {
        serde_json::from_value(json!({
            "type": "object",
            "properties": {
                "duration_ms": {
                    "type": "integer",
                    "description": "Duration to sleep in milliseconds (1–300000). Max 5 minutes.",
                    "minimum": 1,
                    "maximum": 300000
                }
            },
            "required": ["duration_ms"]
        }))
        .unwrap()
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> CcResult<ToolResult> {
        let ms = match input.get("duration_ms").and_then(Value::as_u64) {
            Some(n) => n.min(300_000),
            None => return Ok(ToolResult::error("missing required field: duration_ms")),
        };

        let dur = std::time::Duration::from_millis(ms);

        tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => {
                Err(CcError::Cancelled)
            }
            _ = tokio::time::sleep(dur) => {
                Ok(ToolResult::ok(format!("Slept for {ms}ms")))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    fn ctx() -> ToolContext {
        ToolContext::for_test_bare(CancellationToken::new())
    }

    #[tokio::test]
    async fn sleeps_for_duration() {
        let tool = SleepTool;
        let start = std::time::Instant::now();
        let r = tool
            .execute(json!({"duration_ms": 50}), &ctx())
            .await
            .unwrap();
        assert!(!r.is_error);
        assert!(r.content.contains("50ms"));
        assert!(start.elapsed() >= std::time::Duration::from_millis(50));
    }

    #[tokio::test]
    async fn missing_duration_returns_error() {
        let tool = SleepTool;
        let r = tool.execute(json!({}), &ctx()).await.unwrap();
        assert!(r.is_error);
    }

    #[tokio::test]
    async fn cancelled_returns_err() {
        let tool = SleepTool;
        let token = CancellationToken::new();
        token.cancel();
        let ctx = ToolContext::for_test_bare(token);
        let r = tool.execute(json!({"duration_ms": 5000}), &ctx).await;
        assert!(matches!(r, Err(CcError::Cancelled)));
    }
}
