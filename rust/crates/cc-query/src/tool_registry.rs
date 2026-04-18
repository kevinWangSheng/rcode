//! Central registry of all available tools (built-in + MCP) (§4.4).

use cc_core::ToolDefinition;
use cc_tools::Tool;
use std::collections::HashMap;
use std::sync::Arc;

/// A type-erased tool reference.
pub type BoxTool = Arc<dyn Tool>;

/// Central registry of all available tools (built-in + MCP).
pub struct ToolRegistry {
    tools: HashMap<String, BoxTool>,
    /// Ordered list for API tool definitions (preserves registration order).
    ordered: Vec<String>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
            ordered: Vec::new(),
        }
    }

    pub fn register(&mut self, tool: BoxTool) {
        let name = tool.name().to_string();
        self.ordered.push(name.clone());
        self.tools.insert(name, tool);
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(|t| t.as_ref())
    }

    pub fn get_arc(&self, name: &str) -> Option<&BoxTool> {
        self.tools.get(name)
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.ordered
            .iter()
            .filter_map(|n| self.tools.get(n))
            .map(|t| t.to_definition())
            .collect()
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use cc_core::{CcResult, ToolInputSchema};
    use cc_tools::ToolResult;
    use serde_json::Value;
    use tokio_util::sync::CancellationToken;

    struct DummyTool {
        tool_name: String,
        read_only: bool,
    }

    #[async_trait]
    impl Tool for DummyTool {
        fn name(&self) -> &str {
            &self.tool_name
        }
        fn description(&self) -> &str {
            "dummy"
        }
        fn input_schema(&self) -> ToolInputSchema {
            ToolInputSchema {
                kind: "object".into(),
                properties: None,
                required: None,
                additional_properties: None,
            }
        }
        fn is_read_only(&self) -> bool {
            self.read_only
        }
        async fn execute(
            &self,
            _input: Value,
            _cancel: &CancellationToken,
        ) -> CcResult<ToolResult> {
            Ok(ToolResult::ok("ok"))
        }
    }

    #[test]
    fn register_and_lookup() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(DummyTool {
            tool_name: "Read".into(),
            read_only: true,
        }));
        reg.register(Arc::new(DummyTool {
            tool_name: "Write".into(),
            read_only: false,
        }));

        assert_eq!(reg.len(), 2);
        assert!(reg.get("Read").unwrap().is_read_only());
        assert!(!reg.get("Write").unwrap().is_read_only());
        assert!(reg.get("Missing").is_none());
    }

    #[test]
    fn definitions_preserve_order() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(DummyTool {
            tool_name: "B".into(),
            read_only: false,
        }));
        reg.register(Arc::new(DummyTool {
            tool_name: "A".into(),
            read_only: false,
        }));

        let defs = reg.definitions();
        assert_eq!(defs[0].name, "B");
        assert_eq!(defs[1].name, "A");
    }
}
