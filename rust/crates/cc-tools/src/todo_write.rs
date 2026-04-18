//! TodoWrite — write or update the session todo list.
//!
//! A simpler alternative to the TaskCreate/Update system for quick session-level
//! task tracking. Replaces the entire list on each call.

use async_trait::async_trait;
use cc_core::CcResult;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolInputSchema, ToolResult};

/// A single item in the TodoWrite list.
#[derive(Debug, Clone)]
pub struct TodoItem {
    pub content: String,
    pub status: TodoItemStatus,
    pub priority: TodoItemPriority,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TodoItemStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone)]
pub enum TodoItemPriority {
    High,
    Medium,
    Low,
}

/// Shared todo write list used by TodoWriteTool.
#[derive(Debug, Default)]
pub struct TodoWriteList {
    pub items: Vec<TodoItem>,
}

impl TodoWriteList {
    pub fn new() -> Self {
        Self { items: Vec::new() }
    }
}

pub struct TodoWriteTool {
    pub list: Arc<Mutex<TodoWriteList>>,
}

fn parse_status(s: &str) -> TodoItemStatus {
    match s {
        "in_progress" => TodoItemStatus::InProgress,
        "completed" => TodoItemStatus::Completed,
        _ => TodoItemStatus::Pending,
    }
}

fn parse_priority(s: &str) -> TodoItemPriority {
    match s {
        "high" => TodoItemPriority::High,
        "low" => TodoItemPriority::Low,
        _ => TodoItemPriority::Medium,
    }
}

fn status_str(s: &TodoItemStatus) -> &'static str {
    match s {
        TodoItemStatus::Pending => "pending",
        TodoItemStatus::InProgress => "in_progress",
        TodoItemStatus::Completed => "completed",
    }
}

#[async_trait]
impl Tool for TodoWriteTool {
    fn name(&self) -> &str {
        "TodoWrite"
    }

    fn description(&self) -> &str {
        "Create and manage a structured task list for the current session. Use this to \
         track progress on multi-step tasks, demonstrate thoroughness, and help the user \
         understand overall progress. Replace the entire list on each call."
    }

    fn input_schema(&self) -> ToolInputSchema {
        serde_json::from_value(json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "description": "The complete updated todo list",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": {
                                "type": "string",
                                "description": "Unique identifier for this todo item"
                            },
                            "content": {
                                "type": "string",
                                "description": "Description of the task"
                            },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed"],
                                "description": "Current status of the task"
                            },
                            "priority": {
                                "type": "string",
                                "enum": ["high", "medium", "low"],
                                "description": "Priority of the task"
                            }
                        },
                        "required": ["id", "content", "status", "priority"]
                    }
                }
            },
            "required": ["todos"]
        }))
        .unwrap()
    }

    async fn execute(&self, input: Value, _cancel: &CancellationToken) -> CcResult<ToolResult> {
        let todos_arr = match input.get("todos").and_then(Value::as_array) {
            Some(a) => a.clone(),
            None => return Ok(ToolResult::error("missing required field: todos")),
        };

        let mut new_items = Vec::new();
        for item in &todos_arr {
            let content = match item.get("content").and_then(Value::as_str) {
                Some(c) => c.to_string(),
                None => return Ok(ToolResult::error("each todo must have a 'content' field")),
            };
            let status = item
                .get("status")
                .and_then(Value::as_str)
                .map(parse_status)
                .unwrap_or(TodoItemStatus::Pending);
            let priority = item
                .get("priority")
                .and_then(Value::as_str)
                .map(parse_priority)
                .unwrap_or(TodoItemPriority::Medium);
            new_items.push(TodoItem {
                content,
                status,
                priority,
            });
        }

        // Snapshot old list before replacing
        let old_items = {
            let mut list = self.list.lock().unwrap();
            let old = list.items.clone();
            list.items = new_items.clone();
            old
        };

        // Build result
        let old_json: Vec<Value> = old_items
            .iter()
            .map(|i| json!({"content": i.content, "status": status_str(&i.status)}))
            .collect();
        let new_json: Vec<Value> = new_items
            .iter()
            .map(|i| json!({"content": i.content, "status": status_str(&i.status)}))
            .collect();

        let result = json!({
            "oldTodos": old_json,
            "newTodos": new_json,
        });
        Ok(ToolResult::ok(result.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tool() -> TodoWriteTool {
        TodoWriteTool {
            list: Arc::new(Mutex::new(TodoWriteList::new())),
        }
    }

    #[tokio::test]
    async fn writes_todo_list() {
        let tool = make_tool();
        let r = tool
            .execute(
                json!({
                    "todos": [
                        {"id": "1", "content": "Write tests", "status": "pending", "priority": "high"},
                        {"id": "2", "content": "Fix bug", "status": "in_progress", "priority": "medium"}
                    ]
                }),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(!r.is_error);
        assert!(r.content.contains("newTodos"));
        let list = tool.list.lock().unwrap();
        assert_eq!(list.items.len(), 2);
        assert_eq!(list.items[1].status, TodoItemStatus::InProgress);
    }

    #[tokio::test]
    async fn missing_todos_returns_error() {
        let tool = make_tool();
        let r = tool
            .execute(json!({}), &CancellationToken::new())
            .await
            .unwrap();
        assert!(r.is_error);
    }
}
