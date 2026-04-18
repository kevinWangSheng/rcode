//! ToolSearch — fetch full schema definitions for deferred/unknown tools.
//!
//! Allows the model to discover and load tool schemas on demand without
//! having every tool's definition in the initial system prompt. Takes a
//! query (exact names or keyword search) and returns matched tool definitions
//! inside a `<functions>` block.

use std::sync::Arc;

use async_trait::async_trait;
use cc_core::CcResult;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolInputSchema, ToolResult};

/// A simplified view of a registered tool for search purposes.
pub struct ToolEntry {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

/// Callback used to enumerate available tools (injected from main.rs /
/// cc-query to avoid circular dependencies).
pub type ToolLister = Arc<dyn Fn() -> Vec<ToolEntry> + Send + Sync>;

pub struct ToolSearchTool {
    /// Lists all available tools. Injected at startup.
    pub list_tools: Option<ToolLister>,
}

#[async_trait]
impl Tool for ToolSearchTool {
    fn name(&self) -> &str {
        "ToolSearch"
    }

    fn description(&self) -> &str {
        "Fetches full schema definitions for deferred tools so they can be called. \
         Deferred tools appear by name in <system-reminder> messages. Until fetched, \
         only the name is known — there is no parameter schema, so the tool cannot be \
         invoked. This tool takes a query, matches it against the available tool list, \
         and returns matched tools' complete JSONSchema definitions inside a <functions> \
         block. Once a tool's schema appears in that result, it is callable. \
         Query forms: \"select:Read,Edit,Grep\" (exact names), \"notebook jupyter\" (keywords), \
         \"+slack send\" (require term in name, rank by remainder)."
    }

    fn input_schema(&self) -> ToolInputSchema {
        serde_json::from_value(json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query. Use 'select:Name1,Name2' for exact names, \
                                    or keywords to search descriptions."
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default: 5).",
                    "default": 5
                }
            },
            "required": ["query"]
        }))
        .unwrap()
    }

    async fn execute(&self, input: Value, _cancel: &CancellationToken) -> CcResult<ToolResult> {
        let query = match input.get("query").and_then(Value::as_str) {
            Some(q) => q.to_string(),
            None => return Ok(ToolResult::error("missing required field: query")),
        };
        let max_results = input
            .get("max_results")
            .and_then(Value::as_u64)
            .unwrap_or(5) as usize;

        let list_fn = match &self.list_tools {
            Some(f) => f,
            None => {
                return Ok(ToolResult::error(
                    "ToolSearch is not available: tool lister not wired at startup",
                ))
            }
        };

        let tools = list_fn();

        // Parse query
        let matches = if let Some(names_str) = query.strip_prefix("select:") {
            // Exact name selection
            let names: Vec<&str> = names_str.split(',').map(str::trim).collect();
            tools
                .into_iter()
                .filter(|t| names.contains(&t.name.as_str()))
                .take(max_results)
                .collect::<Vec<_>>()
        } else if let Some(required_and_rest) = query.strip_prefix('+') {
            // +required keywords
            let parts: Vec<&str> = required_and_rest.splitn(2, ' ').collect();
            let required = parts[0].to_lowercase();
            let rest = parts.get(1).copied().unwrap_or("").to_lowercase();
            let mut scored: Vec<(usize, ToolEntry)> = tools
                .into_iter()
                .filter(|t| t.name.to_lowercase().contains(&required))
                .map(|t| {
                    let score = if rest.is_empty() {
                        0
                    } else {
                        let haystack = format!("{} {}", t.name, t.description).to_lowercase();
                        rest.split_whitespace()
                            .filter(|w| haystack.contains(*w))
                            .count()
                    };
                    (score, t)
                })
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0));
            scored
                .into_iter()
                .map(|(_, t)| t)
                .take(max_results)
                .collect()
        } else {
            // Keyword search — rank by matches in name + description
            let keywords: Vec<String> =
                query.split_whitespace().map(|s| s.to_lowercase()).collect();
            let mut scored: Vec<(usize, ToolEntry)> = tools
                .into_iter()
                .filter_map(|t| {
                    let haystack = format!("{} {}", t.name, t.description).to_lowercase();
                    let score = keywords
                        .iter()
                        .filter(|kw| haystack.contains(kw.as_str()))
                        .count();
                    if score > 0 {
                        Some((score, t))
                    } else {
                        None
                    }
                })
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0));
            scored
                .into_iter()
                .map(|(_, t)| t)
                .take(max_results)
                .collect()
        };

        if matches.is_empty() {
            return Ok(ToolResult::ok(format!(
                "No tools found matching query: {query}"
            )));
        }

        // Format as <functions> block (same encoding as initial prompt)
        let mut lines = vec!["<functions>".to_string()];
        for tool in &matches {
            let def = json!({
                "description": tool.description,
                "name": tool.name,
                "parameters": tool.schema,
            });
            lines.push(format!(
                "<function>{}</function>",
                serde_json::to_string(&def).unwrap()
            ));
        }
        lines.push("</functions>".to_string());

        Ok(ToolResult::ok(lines.join("\n")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tool(entries: Vec<ToolEntry>) -> ToolSearchTool {
        ToolSearchTool {
            list_tools: Some(Arc::new(move || {
                entries
                    .iter()
                    .map(|e| ToolEntry {
                        name: e.name.clone(),
                        description: e.description.clone(),
                        schema: e.schema.clone(),
                    })
                    .collect()
            })),
        }
    }

    fn sample_tools() -> Vec<ToolEntry> {
        vec![
            ToolEntry {
                name: "Read".into(),
                description: "Read a file".into(),
                schema: json!({"type": "object"}),
            },
            ToolEntry {
                name: "Write".into(),
                description: "Write a file".into(),
                schema: json!({"type": "object"}),
            },
            ToolEntry {
                name: "Bash".into(),
                description: "Run a shell command".into(),
                schema: json!({"type": "object"}),
            },
        ]
    }

    #[tokio::test]
    async fn select_exact_names() {
        let tool = make_tool(sample_tools());
        let r = tool
            .execute(
                json!({"query": "select:Read,Write"}),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(!r.is_error);
        assert!(r.content.contains("<functions>"));
        assert!(r.content.contains("\"name\":\"Read\""));
        assert!(r.content.contains("\"name\":\"Write\""));
        assert!(!r.content.contains("\"name\":\"Bash\""));
    }

    #[tokio::test]
    async fn keyword_search() {
        let tool = make_tool(sample_tools());
        let r = tool
            .execute(json!({"query": "file"}), &CancellationToken::new())
            .await
            .unwrap();
        assert!(!r.is_error);
        // "Read a file" and "Write a file" both match
        assert!(r.content.contains("Read") || r.content.contains("Write"));
    }

    #[tokio::test]
    async fn no_match_returns_message() {
        let tool = make_tool(sample_tools());
        let r = tool
            .execute(
                json!({"query": "nonexistent_xyz"}),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(!r.is_error);
        assert!(r.content.contains("No tools found"));
    }

    #[tokio::test]
    async fn not_wired_returns_error() {
        let tool = ToolSearchTool { list_tools: None };
        let r = tool
            .execute(json!({"query": "read"}), &CancellationToken::new())
            .await
            .unwrap();
        assert!(r.is_error);
    }
}
