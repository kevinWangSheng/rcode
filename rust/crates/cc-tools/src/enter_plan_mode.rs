//! EnterPlanMode — requests permission to enter plan mode.
//!
//! Plan mode is a state where the assistant focuses on exploration and
//! design before committing to implementation. This tool signals the
//! transition and requests user confirmation.

use async_trait::async_trait;
use cc_core::CcResult;
use serde_json::{json, Value};

use crate::{Tool, ToolContext, ToolInputSchema, ToolResult};

pub struct EnterPlanModeTool;

#[async_trait]
impl Tool for EnterPlanModeTool {
    fn name(&self) -> &str {
        "EnterPlanMode"
    }

    fn description(&self) -> &str {
        "Requests permission to enter plan mode for complex tasks requiring exploration \
         and design. In plan mode, the assistant focuses on understanding the problem \
         and designing a solution before writing any code. Call this before tackling \
         large or ambiguous tasks."
    }

    fn input_schema(&self) -> ToolInputSchema {
        serde_json::from_value(json!({
            "type": "object",
            "properties": {}
        }))
        .unwrap()
    }

    async fn execute(&self, _input: Value, _ctx: &ToolContext) -> CcResult<ToolResult> {
        Ok(ToolResult::ok(
            "Entered plan mode. You are now in exploration/design mode. \
             Focus on understanding the problem, asking clarifying questions, \
             and designing a solution before writing code. \
             Call ExitPlanMode when you are ready to implement.",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn enters_plan_mode() {
        let tool = EnterPlanModeTool;
        let r = tool
            .execute(
                json!({}),
                &ToolContext::for_test_bare(CancellationToken::new()),
            )
            .await
            .unwrap();
        assert!(!r.is_error);
        assert!(r.content.contains("plan mode"));
    }
}
