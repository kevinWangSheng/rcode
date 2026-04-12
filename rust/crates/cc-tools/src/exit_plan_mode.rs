//! ExitPlanMode — exits plan mode and returns to implementation mode.

use async_trait::async_trait;
use cc_core::CcResult;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolInputSchema, ToolResult};

pub struct ExitPlanModeTool;

#[async_trait]
impl Tool for ExitPlanModeTool {
    fn name(&self) -> &str {
        "ExitPlanMode"
    }

    fn description(&self) -> &str {
        "Exits plan mode and returns to implementation mode. Call this when you have \
         finished designing the solution and are ready to write code. After calling \
         this tool you should proceed with implementation."
    }

    fn input_schema(&self) -> ToolInputSchema {
        serde_json::from_value(json!({
            "type": "object",
            "properties": {}
        }))
        .unwrap()
    }

    async fn execute(&self, _input: Value, _cancel: &CancellationToken) -> CcResult<ToolResult> {
        Ok(ToolResult::ok(
            "Exited plan mode. You are now in implementation mode. \
             Proceed with writing code to implement the designed solution.",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn exits_plan_mode() {
        let tool = ExitPlanModeTool;
        let r = tool
            .execute(json!({}), &CancellationToken::new())
            .await
            .unwrap();
        assert!(!r.is_error);
        assert!(r.content.contains("implementation mode"));
    }
}
