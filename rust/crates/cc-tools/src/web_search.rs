//! WebSearch — query a search engine and return result snippets.
//!
//! Backend: Brave Search API (https://api.search.brave.com). Requires the
//! `BRAVE_SEARCH_API_KEY` environment variable. We chose Brave because it has
//! a free tier, doesn't require Google Cloud setup, and returns structured
//! JSON results.
//!
//! Note: this is intentionally not the same shape as the TS Claude Code
//! WebSearch tool, which uses Anthropic's *server-side* web_search tool type
//! (handled transparently by the Messages API). Supporting that would require
//! teaching `ToolDefinition` about server-side tool variants — a larger
//! refactor than the DoC gap-closing scope. For now this local implementation
//! gives the model a working web search, and the upgrade path to server-side
//! is documented in implementation-notes.md.

use async_trait::async_trait;
use cc_core::{CcError, CcResult};
use serde_json::{json, Value};

use crate::{Tool, ToolResult};

const BRAVE_SEARCH_URL: &str = "https://api.search.brave.com/res/v1/web/search";
const REQUEST_TIMEOUT_SECS: u64 = 30;
const DEFAULT_RESULT_COUNT: u32 = 10;
const MAX_RESULT_COUNT: u32 = 20;

pub struct WebSearchTool;

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "WebSearch"
    }

    fn description(&self) -> &str {
        "Search the web and return a list of result snippets (title, url, description). \
         Use for finding documentation, recent news, or any information not in your training data. \
         Backed by the Brave Search API; requires BRAVE_SEARCH_API_KEY in the environment."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query"
                },
                "count": {
                    "type": "integer",
                    "description": "Number of results to return (1-20, default 10)"
                }
            },
            "required": ["query"]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value) -> CcResult<ToolResult> {
        let query = input["query"]
            .as_str()
            .ok_or_else(|| CcError::Tool("missing 'query' field".into()))?
            .to_string();

        if query.trim().is_empty() {
            return Ok(ToolResult::error("WebSearch: empty query".to_string()));
        }

        let count = input["count"]
            .as_u64()
            .map(|n| (n as u32).clamp(1, MAX_RESULT_COUNT))
            .unwrap_or(DEFAULT_RESULT_COUNT);

        let api_key = match std::env::var("BRAVE_SEARCH_API_KEY") {
            Ok(k) if !k.is_empty() => k,
            _ => {
                return Ok(ToolResult::error(
                    "WebSearch is not configured: set BRAVE_SEARCH_API_KEY environment \
                     variable to a Brave Search API key (https://api.search.brave.com)."
                        .to_string(),
                ));
            }
        };

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .user_agent("claude-code-rust/0.1")
            .build()
            .map_err(|e| CcError::Tool(format!("failed to build http client: {e}")))?;

        let response = match client
            .get(BRAVE_SEARCH_URL)
            .header("X-Subscription-Token", api_key)
            .header("Accept", "application/json")
            .query(&[("q", query.as_str()), ("count", &count.to_string())])
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return Ok(ToolResult::error(format!("WebSearch request failed: {e}"))),
        };

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Ok(ToolResult::error(format!(
                "WebSearch API returned HTTP {status}: {body}"
            )));
        }

        let body: Value = match response.json().await {
            Ok(v) => v,
            Err(e) => {
                return Ok(ToolResult::error(format!(
                    "WebSearch: failed to parse JSON response: {e}"
                )));
            }
        };

        let formatted = format_brave_results(&body, &query);
        Ok(ToolResult::ok(formatted))
    }
}

/// Format a Brave Search API JSON response into a compact text block the model
/// can read. Brave's response shape:
/// `{ "web": { "results": [{ "title", "url", "description", ... }, ...] } }`
fn format_brave_results(body: &Value, query: &str) -> String {
    let results = body
        .get("web")
        .and_then(|w| w.get("results"))
        .and_then(|r| r.as_array());

    let Some(results) = results else {
        return format!("Search query: {query}\n(no results)");
    };

    if results.is_empty() {
        return format!("Search query: {query}\n(no results)");
    }

    let mut out = format!("Search query: {query}\n\n");
    for (i, item) in results.iter().enumerate() {
        let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("(no title)");
        let url = item.get("url").and_then(|v| v.as_str()).unwrap_or("");
        let desc = item
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        out.push_str(&format!("{}. {title}\n   {url}\n   {desc}\n\n", i + 1));
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn execute_missing_query_errors() {
        let tool = WebSearchTool;
        let err = tool.execute(json!({})).await.unwrap_err();
        assert!(err.to_string().contains("query"));
    }

    #[tokio::test]
    async fn execute_empty_query_returns_error_result() {
        let tool = WebSearchTool;
        let result = tool.execute(json!({"query": "   "})).await.unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("empty"));
    }

    #[tokio::test]
    async fn execute_without_api_key_returns_clear_error() {
        // SAFETY: tests run sequentially within a process for env var manipulation;
        // this set/remove pair is contained in this test.
        std::env::remove_var("BRAVE_SEARCH_API_KEY");
        let tool = WebSearchTool;
        let result = tool
            .execute(json!({"query": "rust async"}))
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("BRAVE_SEARCH_API_KEY"));
    }

    #[test]
    fn format_brave_results_renders_items() {
        let body = json!({
            "web": {
                "results": [
                    {"title": "Rust", "url": "https://rust-lang.org", "description": "A language"},
                    {"title": "Tokio", "url": "https://tokio.rs", "description": "Async runtime"}
                ]
            }
        });
        let out = format_brave_results(&body, "rust");
        assert!(out.contains("1. Rust"));
        assert!(out.contains("https://rust-lang.org"));
        assert!(out.contains("2. Tokio"));
    }

    #[test]
    fn format_brave_results_handles_empty() {
        let body = json!({"web": {"results": []}});
        let out = format_brave_results(&body, "x");
        assert!(out.contains("(no results)"));
    }
}
