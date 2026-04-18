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

pub mod ssrf;

use async_trait::async_trait;
use cc_core::{CcError, CcResult};
use serde_json::{json, Value};

use crate::{Tool, ToolInputSchema, ToolResult};
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
        }))
        .unwrap()
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

        // SSRF guard: resolve the hostname and reject any address pointing at
        // loopback, RFC1918, link-local (including 169.254.169.254 cloud
        // metadata), CGNAT, or unique-local IPv6. Returns the resolved
        // socket address so we can pin reqwest to that IP and defeat DNS
        // rebinding between the check and the fetch.
        let parsed = url::Url::parse(&url)
            .map_err(|e| CcError::tool("tool", format!("WebFetch refused: invalid URL: {e}")))?;
        let guard = match ssrf::guard_url(&parsed).await {
            Ok(g) => g,
            Err(e) => {
                // Return as a tool error (not an Err) so the model sees a
                // clear refusal and doesn't try a different encoding.
                return Ok(ToolResult::error(e.to_string()));
            }
        };

        let mut builder = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .user_agent("claude-code-rust/0.1");

        // Pin the outbound connection to the IP we just vetted. Even if the
        // authoritative DNS server rebinds between guard_url and send(),
        // reqwest will still connect to this address.
        if let Some(host) = parsed.host_str() {
            builder = builder.resolve(host, guard.resolved);
        }

        let client = builder
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
        //
        // NOTE: after the SSRF fix, WebFetch refuses 127.0.0.1 by default.
        // Set the opt-out env var so we can still exercise the cancel path
        // against a local test server.
        use tokio::net::TcpListener;
        // Serialize with any other test that mutates CC_WEBFETCH_ALLOW_PRIVATE.
        let _lock = crate::web_fetch::ssrf::ENV_LOCK.lock().await;
        std::env::set_var("CC_WEBFETCH_ALLOW_PRIVATE", "1");
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
        let result = tool.execute(json!({"url": url}), &cancel).await;
        std::env::remove_var("CC_WEBFETCH_ALLOW_PRIVATE");
        let err = result.unwrap_err();
        let elapsed = start.elapsed();
        assert!(err.to_string().contains("cancelled"), "got: {err}");
        // Must have bailed well before the 30s reqwest timeout.
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "took {:?}",
            elapsed
        );
    }

    #[tokio::test]
    async fn execute_refuses_aws_metadata_address() {
        // Crucial test: http://169.254.169.254/... must be refused BEFORE any
        // socket is opened. We use a connection counter via a sentinel
        // TcpListener bound on a different IP; if our code were to actually
        // attempt the connect, we'd see a TCP-level error rather than our
        // refusal message. Either way the content must contain "refused".
        let _lock = crate::web_fetch::ssrf::ENV_LOCK.lock().await;
        std::env::remove_var("CC_WEBFETCH_ALLOW_PRIVATE");
        let tool = WebFetchTool;
        let cancel = CancellationToken::new();
        let result = tool
            .execute(
                json!({"url": "http://169.254.169.254/latest/meta-data/iam/"}),
                &cancel,
            )
            .await
            .unwrap();
        assert!(result.is_error, "metadata endpoint must be refused");
        assert!(
            result.content.contains("refused") && result.content.contains("169.254"),
            "error should mention refusal + host: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn execute_refuses_local_http_server() {
        // Boot a local HTTP server on a random loopback port and assert that
        // WebFetch refuses it by default (no opt-out env var).
        use tokio::net::TcpListener;
        let _lock = crate::web_fetch::ssrf::ENV_LOCK.lock().await;
        std::env::remove_var("CC_WEBFETCH_ALLOW_PRIVATE");
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let accepts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let accepts2 = accepts.clone();
        let _server = tokio::spawn(async move {
            loop {
                if let Ok((sock, _)) = listener.accept().await {
                    accepts2.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    drop(sock);
                }
            }
        });

        let tool = WebFetchTool;
        let cancel = CancellationToken::new();
        let url = format!("http://127.0.0.1:{port}/");
        let result = tool.execute(json!({"url": url}), &cancel).await.unwrap();
        assert!(result.is_error, "loopback fetch must be refused");
        assert!(
            result.content.contains("refused"),
            "got: {}",
            result.content
        );
        // Critical: no socket was opened. The counter must stay at 0.
        assert_eq!(
            accepts.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "guard must refuse BEFORE opening a socket"
        );
    }

    #[tokio::test]
    async fn execute_refuses_rfc1918_host() {
        // Direct literal-IP check — we can't hit the box so there's no risk
        // of flakiness from DNS or network.
        let _lock = crate::web_fetch::ssrf::ENV_LOCK.lock().await;
        std::env::remove_var("CC_WEBFETCH_ALLOW_PRIVATE");
        let tool = WebFetchTool;
        let cancel = CancellationToken::new();
        let result = tool
            .execute(json!({"url": "http://10.0.0.1/admin"}), &cancel)
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("refused"));
    }

    #[tokio::test]
    async fn execute_refuses_ipv6_loopback() {
        let _lock = crate::web_fetch::ssrf::ENV_LOCK.lock().await;
        std::env::remove_var("CC_WEBFETCH_ALLOW_PRIVATE");
        let tool = WebFetchTool;
        let cancel = CancellationToken::new();
        let result = tool
            .execute(json!({"url": "http://[::1]:8080/"}), &cancel)
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("refused"));
    }
}
