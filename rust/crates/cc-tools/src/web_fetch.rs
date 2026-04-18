//! WebFetch — GET a URL, return its content as text.
//!
//! The TS Claude Code WebFetch tool also runs the body through a small model
//! with a `prompt` field to summarize. We accept the `prompt` field for schema
//! compatibility but currently return the raw fetched content; LLM-based
//! summarization is a follow-up that depends on cc-api access from inside the
//! tool layer (currently the tool layer is independent of cc-api by design).
//!
//! Safety: only `http://` and `https://` URLs are allowed. Response bodies are
//! capped at `MAX_RESPONSE_BYTES`. HTML responses are stripped of tags via a
//! lightweight regex pass — we deliberately avoid pulling a full HTML parser
//! crate for this MVP.

use async_trait::async_trait;
use cc_core::{CcError, CcResult};
use serde_json::{json, Value};

use crate::{Tool, ToolResult, ToolInputSchema};
use tokio_util::sync::CancellationToken;

const MAX_RESPONSE_BYTES: usize = 1_000_000; // 1 MB
const REQUEST_TIMEOUT_SECS: u64 = 30;

pub struct WebFetchTool;

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "WebFetch"
    }

    fn description(&self) -> &str {
        "Fetch the contents of a URL and return them as text. \
         Use for reading documentation pages, API references, blog posts, or any \
         publicly accessible web content. HTML responses are stripped of tags. \
         Response bodies are capped at 1 MB; only http and https URLs are allowed."
    }

    fn input_schema(&self) -> ToolInputSchema {
        serde_json::from_value(json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "Absolute URL to fetch (http:// or https://)"
                },
                "prompt": {
                    "type": "string",
                    "description": "Optional summarization prompt (reserved; currently ignored)"
                }
            },
            "required": ["url"]
        })).unwrap()
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, cancel: &CancellationToken) -> CcResult<ToolResult> {
        let url = input["url"]
            .as_str()
            .ok_or_else(|| CcError::tool("tool", "missing 'url' field"))?
            .to_string();

        if !is_allowed_url(&url) {
            return Ok(ToolResult::error(format!(
                "WebFetch refused: only http:// and https:// URLs are allowed (got {url})"
            )));
        }

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .user_agent("claude-code-rust/0.1")
            .build()
            .map_err(|e| CcError::tool("tool", format!("failed to build http client: {e}")))?;

        // Race the fetch against the cancel token so Ctrl+C can abort a
        // hung / slow server without waiting the full 30s reqwest timeout.
        let response = tokio::select! {
            r = client.get(&url).send() => match r {
                Ok(r) => r,
                Err(e) => return Ok(ToolResult::error(format!("WebFetch request failed: {e}"))),
            },
            _ = cancel.cancelled() => {
                return Err(CcError::tool("tool", "WebFetch cancelled"));
            }
        };

        let status = response.status();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_lowercase();

        // Use bytes() so we can enforce the size cap before allocating a String.
        // Same cancel race — the body read can also hang on a slow server.
        let bytes = tokio::select! {
            b = response.bytes() => match b {
                Ok(b) => b,
                Err(e) => return Ok(ToolResult::error(format!("WebFetch read failed: {e}"))),
            },
            _ = cancel.cancelled() => {
                return Err(CcError::tool("tool", "WebFetch cancelled"));
            }
        };

        let truncated = bytes.len() > MAX_RESPONSE_BYTES;
        let slice = &bytes[..bytes.len().min(MAX_RESPONSE_BYTES)];
        let body = String::from_utf8_lossy(slice).to_string();

        let body = if content_type.contains("html") {
            strip_html(&body)
        } else {
            body
        };

        let header = format!("HTTP {status} {url}\nContent-Type: {content_type}\n");
        let footer = if truncated {
            format!("\n\n[truncated at {MAX_RESPONSE_BYTES} bytes]")
        } else {
            String::new()
        };

        let content = format!("{header}\n{body}{footer}");

        if status.is_success() {
            Ok(ToolResult::ok(content))
        } else {
            // Non-2xx is surfaced as an error result so the model knows the
            // page wasn't really fetched, but we still hand it the body in case
            // it's a useful 404 page or similar.
            Ok(ToolResult::error(content))
        }
    }
}

fn is_allowed_url(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

/// Strip HTML tags, script/style blocks, and collapse whitespace. Deliberately
/// minimal — for full fidelity callers should use the eventual LLM
/// summarization path.
fn strip_html(html: &str) -> String {
    // Drop <script>...</script> and <style>...</style> entirely. We do two
    // passes because the `regex` crate has no backreference support.
    let drop_script = regex::Regex::new(r"(?is)<script[^>]*>.*?</script>").unwrap();
    let no_script = drop_script.replace_all(html, " ");
    let drop_style = regex::Regex::new(r"(?is)<style[^>]*>.*?</style>").unwrap();
    let no_blocks = drop_style.replace_all(&no_script, " ");

    // Drop all remaining tags.
    let drop_tags = regex::Regex::new(r"(?s)<[^>]+>").unwrap();
    let no_tags = drop_tags.replace_all(&no_blocks, " ");

    // Decode a few common entities; we don't pull a full entity-decoder.
    let decoded = no_tags
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ");

    // Collapse runs of whitespace.
    let ws = regex::Regex::new(r"\s+").unwrap();
    ws.replace_all(&decoded, " ").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_http_urls() {
        assert!(!is_allowed_url("file:///etc/passwd"));
        assert!(!is_allowed_url("ftp://example.com"));
        assert!(!is_allowed_url("javascript:alert(1)"));
        assert!(is_allowed_url("http://example.com"));
        assert!(is_allowed_url("https://example.com/page?x=1"));
    }

    #[test]
    fn strips_html_basic_tags() {
        let html = "<html><body><h1>Hello</h1><p>World &amp; friends</p></body></html>";
        let stripped = strip_html(html);
        assert_eq!(stripped, "Hello World & friends");
    }

    #[test]
    fn strips_html_drops_script_and_style() {
        let html = "<html><head><style>body{color:red;}</style><script>alert('x')</script></head><body>Visible</body></html>";
        let stripped = strip_html(html);
        assert_eq!(stripped, "Visible");
    }

    #[tokio::test]
    async fn execute_rejects_file_url() {
        let tool = WebFetchTool;
        let cancel = CancellationToken::new();
        let result = tool
            .execute(json!({"url": "file:///etc/passwd"}), &cancel)
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("only http"));
    }

    #[tokio::test]
    async fn execute_missing_url_errors() {
        let tool = WebFetchTool;
        let cancel = CancellationToken::new();
        let err = tool.execute(json!({}), &cancel).await.unwrap_err();
        assert!(err.to_string().contains("url"));
    }

    #[tokio::test]
    async fn execute_honors_cancel_token_on_slow_server() {
        // Spin up a TCP listener that accepts the connection but never sends
        // any response. WebFetch would normally wait the full 30s timeout
        // before erroring; with cancel wiring, 100ms is enough.
        use tokio::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let _hang = tokio::spawn(async move {
            let (_sock, _) = listener.accept().await.unwrap();
            // Hold the socket open forever.
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        });

        let tool = WebFetchTool;
        let cancel = CancellationToken::new();
        let cancel2 = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            cancel2.cancel();
        });

        let url = format!("http://127.0.0.1:{port}/hang");
        let start = std::time::Instant::now();
        let err = tool.execute(json!({"url": url}), &cancel).await.unwrap_err();
        let elapsed = start.elapsed();
        assert!(err.to_string().contains("cancelled"), "got: {err}");
        // Must have bailed well before the 30s reqwest timeout.
        assert!(elapsed < std::time::Duration::from_secs(5), "took {:?}", elapsed);
    }
}
