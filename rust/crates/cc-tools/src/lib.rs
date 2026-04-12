pub mod bash;
pub mod edit;
pub mod glob_tool;
pub mod grep;
pub mod read;
pub mod task_create;
pub mod task_get;
pub mod task_list;
pub mod task_output;
pub mod task_stop;
pub mod task_update;
pub mod todo;
pub mod web_fetch;
pub mod web_search;
pub mod write;

// Re-export cc-core's Tool trait and ToolResult for use by tool implementations.
pub use cc_core::tool::{Tool, ToolResult};
pub use cc_core::{ToolDefinition, ToolInputSchema};
pub use todo::TodoList;

use cc_agents::TaskRegistry;
use std::sync::{Arc, Mutex};

/// Build the default set of stateless built-in tools (8 core tools).
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

/// Build the task management tools (TaskCreate/Update/List/Get) sharing a single list.
///
/// Pass a pre-created `Arc<Mutex<TodoList>>` so the caller can hold a reference to
/// the same list (e.g., for display in a TUI status panel or for injection into other
/// components). Use `Arc::new(Mutex::new(TodoList::new()))` if you don't need the ref.
pub fn task_tools(list: Arc<Mutex<TodoList>>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(task_create::TaskCreateTool { list: list.clone() }),
        Arc::new(task_update::TaskUpdateTool { list: list.clone() }),
        Arc::new(task_list::TaskListTool { list: list.clone() }),
        Arc::new(task_get::TaskGetTool { list }),
    ]
}

/// Build background task management tools (TaskStop, TaskOutput) sharing a TaskRegistry.
///
/// The registry should be the same one used by cc-agents to spawn background tasks.
pub fn background_task_tools(registry: Arc<Mutex<TaskRegistry>>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(task_stop::TaskStopTool { registry: registry.clone() }),
        Arc::new(task_output::TaskOutputTool { registry }),
    ]
}

/// Build all built-in tools: 8 core tools + 4 todo task tools + 2 background task tools.
///
/// Returns the combined tool list, the shared todo list, and the shared task registry.
pub fn all_tools() -> (Vec<Arc<dyn Tool>>, Arc<Mutex<TodoList>>, Arc<Mutex<TaskRegistry>>) {
    let list = Arc::new(Mutex::new(TodoList::new()));
    let registry = Arc::new(Mutex::new(TaskRegistry::new(16)));
    let mut tools = default_tools();
    tools.extend(task_tools(list.clone()));
    tools.extend(background_task_tools(registry.clone()));
    (tools, list, registry)
}
