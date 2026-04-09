pub mod bash;
pub mod edit;
pub mod glob_tool;
pub mod grep;
pub mod read;
pub mod web_fetch;
pub mod web_search;
pub mod write;

// Re-export cc-core's Tool trait and ToolResult for use by tool implementations.
pub use cc_core::tool::{Tool, ToolResult};
pub use cc_core::{ToolDefinition, ToolInputSchema};

use std::sync::Arc;

/// Build the default set of built-in tools.
pub fn default_tools() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(bash::BashTool),
        Arc::new(read::ReadTool),
        Arc::new(write::WriteTool),
        Arc::new(edit::EditTool),
        Arc::new(glob_tool::GlobTool),
        Arc::new(grep::GrepTool),
        Arc::new(web_fetch::WebFetchTool),
        Arc::new(web_search::WebSearchTool),
    ]
}
