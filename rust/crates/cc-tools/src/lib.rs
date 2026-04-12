pub mod agent_tool;
pub mod ask_user_question;
pub mod bash;
pub mod edit;
pub mod enter_plan_mode;
pub mod enter_worktree;
pub mod exit_plan_mode;
pub mod exit_worktree;
pub mod glob_tool;
pub mod grep;
pub mod read;
pub mod send_message;
pub mod sleep_tool;
pub mod task_create;
pub mod task_get;
pub mod task_list;
pub mod task_output;
pub mod task_stop;
pub mod task_update;
pub mod team_create;
pub mod team_delete;
pub mod todo;
pub mod todo_write;
pub mod web_fetch;
pub mod web_search;
pub mod write;

// Re-export cc-core's Tool trait and ToolResult for use by tool implementations.
pub use cc_core::tool::{Tool, ToolResult};
pub use cc_core::{ToolDefinition, ToolInputSchema};
pub use todo::TodoList;
pub use todo_write::TodoWriteList;

use cc_agents::{TaskRegistry, TeammateDirectory};
use std::sync::{Arc, Mutex};

/// Build the default set of stateless built-in tools (13 core tools).
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
        Arc::new(sleep_tool::SleepTool),
        Arc::new(enter_plan_mode::EnterPlanModeTool),
        Arc::new(exit_plan_mode::ExitPlanModeTool),
        Arc::new(enter_worktree::EnterWorktreeTool),
        Arc::new(exit_worktree::ExitWorktreeTool),
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

/// Build swarm communication tools (SendMessage, TeamDelete) sharing a TeammateDirectory.
///
/// TeamCreate is NOT included here because it also needs the SubAgentRunner
/// (injected from main.rs to break the circular dep, same as AgentTool).
pub fn swarm_tools(directory: Arc<Mutex<TeammateDirectory>>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(send_message::SendMessageTool { directory: directory.clone() }),
        Arc::new(team_delete::TeamDeleteTool { directory }),
    ]
}

/// Build all built-in tools: 13 core tools + TodoWrite + 4 task tools + 2 background
/// task tools + 2 swarm tools.
///
/// **Does not include AgentTool, AskUserQuestionTool, or TeamCreateTool** — add them
/// separately after building the ToolRegistry.
///
/// Returns the combined tool list, shared todo list, TodoWrite list, task registry,
/// and teammate directory.
pub fn all_tools() -> (
    Vec<Arc<dyn Tool>>,
    Arc<Mutex<TodoList>>,
    Arc<Mutex<todo_write::TodoWriteList>>,
    Arc<Mutex<TaskRegistry>>,
    Arc<Mutex<TeammateDirectory>>,
) {
    let list = Arc::new(Mutex::new(TodoList::new()));
    let todo_write_list = Arc::new(Mutex::new(todo_write::TodoWriteList::new()));
    let registry = Arc::new(Mutex::new(TaskRegistry::new(16)));
    let directory = Arc::new(Mutex::new(TeammateDirectory::new()));
    let mut tools = default_tools();
    tools.extend(task_tools(list.clone()));
    tools.extend(background_task_tools(registry.clone()));
    tools.extend(swarm_tools(directory.clone()));
    tools.push(Arc::new(todo_write::TodoWriteTool { list: todo_write_list.clone() }));
    (tools, list, todo_write_list, registry, directory)
}
