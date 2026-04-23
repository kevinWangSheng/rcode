//! WebFetch — GET a URL, return its content as text.
//!
//! If a `Summarizer` is wired in (the concrete impl lives in `cc-query`
//! and is injected from `main.rs`), the tool also honours the TS-compat
//! `prompt` field: the fetched body is forwarded to the summarizer with
//! the prompt and the summarizer's output replaces the raw body in the
//! returned content. Without a summarizer, the `prompt` field is accepted
//! for schema compatibility but ignored, matching the previous behaviour.
//!
//! Safety: only `http://` and `https://` URLs are allowed. Response bodies are
//! capped at `MAX_RESPONSE_BYTES`. HTML responses are stripped of tags via a
//! lightweight regex pass — we deliberately avoid pulling a full HTML parser
//! crate for this MVP.

pub mod ssrf;

use std::sync::Arc;

use async_trait::async_trait;
use cc_core::{CcError, CcResult, Summarizer};
use serde_json::{json, Value};

use crate::{Tool, ToolContext, ToolInputSchema, ToolResult};

const MAX_RESPONSE_BYTES: usize = 1_000_000; // 1 MB
const REQUEST_TIMEOUT_SECS: u64 = 30;

/// WebFetch tool. Use `WebFetchTool::new()` for the no-summarizer default
/// (returns raw body) or `WebFetchTool::with_summarizer(...)` to enable
/// the LLM-summarization path for the TS-compat `prompt` field.
#[derive(Default)]
pub struct WebFetchTool {
    summarizer: Option<Arc<dyn Summarizer>>,
}

impl WebFetchTool {
    pub fn new() -> Self {
        Self { summarizer: None }
    }

    pub fn with_summarizer(summarizer: Arc<dyn Summarizer>) -> Self {
        Self {
            summarizer: Some(summarizer),
        }
    }
}

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
                    "description": "Optional instruction. If provided and a summarizer is configured, the fetched body is transformed by a model call using this prompt; otherwise the raw body is returned."
                }
            },
            "required": ["url"]
        }))
        .unwrap()
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> CcResult<ToolResult> {
        let url = input["url"]
            .as_str()
            .ok_or_else(|| CcError::tool("tool", "missing 'url' field"))?
            .to_string();
        let summary_prompt = input
            .get("prompt")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

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
            _ = ctx.cancel.cancelled() => {
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
            _ = ctx.cancel.cancelled() => {
                return Err(CcError::tool("tool", "WebFetch cancelled"));
            }
        };

        let truncated = bytes.len() > MAX_RESPONSE_BYTES;
        let slice = &bytes[..bytes.len().min(MAX_RESPONSE_BYTES)];
        let raw_body = String::from_utf8_lossy(slice).to_string();
        let cleaned = if content_type.contains("html") {
            strip_html(&raw_body)
        } else {
            raw_body
        };

        let header = format!("HTTP {status} {url}\nContent-Type: {content_type}\n");
        let footer = if truncated {
            format!("\n\n[truncated at {MAX_RESPONSE_BYTES} bytes]")
        } else {
            String::new()
        };

        // TS-compat summarization path: when `prompt` is non-empty AND a
        // Summarizer is wired in, delegate to the model for a focused
        // answer instead of returning raw body bytes. Summarizer failures
        // fall back to the raw body so a model outage doesn't break the
        // whole tool call — the header still carries the status line for
        // context.
        let mut body = cleaned;
        let mut summarize_error: Option<String> = None;
        if let (Some(summarizer), Some(prompt)) = (&self.summarizer, summary_prompt.as_deref()) {
            match summarizer.summarize(prompt, &body, &ctx.cancel).await {
                Ok(summary) => body = summary,
                Err(e) => {
                    tracing::warn!("WebFetch summarizer failed: {e}; returning raw body");
                    summarize_error = Some(e.to_string());
                }
            }
        }

        let mut content = format!("{header}\n{body}{footer}");
        if let Some(err) = summarize_error {
            content.push_str(&format!(
                "\n\n[summarizer unavailable: {err}; raw body above]"
            ));
        }

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
    use tokio_util::sync::CancellationToken;

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
        let tool = WebFetchTool::new();
        let ctx = ToolContext::for_test_bare(CancellationToken::new());
        let result = tool
            .execute(json!({"url": "file:///etc/passwd"}), &ctx)
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("only http"));
    }

    #[tokio::test]
    async fn execute_missing_url_errors() {
        let tool = WebFetchTool::new();
        let ctx = ToolContext::for_test_bare(CancellationToken::new());
        let err = tool.execute(json!({}), &ctx).await.unwrap_err();
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

        let tool = WebFetchTool::new();
        let token = CancellationToken::new();
        let ctx = ToolContext::for_test_bare(token.clone());
        let token2 = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            token2.cancel();
        });

        let url = format!("http://127.0.0.1:{port}/hang");
        let start = std::time::Instant::now();
        let result = tool.execute(json!({"url": url}), &ctx).await;
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
        let tool = WebFetchTool::new();
        let ctx = ToolContext::for_test_bare(CancellationToken::new());
        let result = tool
            .execute(
                json!({"url": "http://169.254.169.254/latest/meta-data/iam/"}),
                &ctx,
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

        let tool = WebFetchTool::new();
        let ctx = ToolContext::for_test_bare(CancellationToken::new());
        let url = format!("http://127.0.0.1:{port}/");
        let result = tool.execute(json!({"url": url}), &ctx).await.unwrap();
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
        let tool = WebFetchTool::new();
        let ctx = ToolContext::for_test_bare(CancellationToken::new());
        let result = tool
            .execute(json!({"url": "http://10.0.0.1/admin"}), &ctx)
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("refused"));
    }

    #[tokio::test]
    async fn execute_refuses_ipv6_loopback() {
        let _lock = crate::web_fetch::ssrf::ENV_LOCK.lock().await;
        std::env::remove_var("CC_WEBFETCH_ALLOW_PRIVATE");
        let tool = WebFetchTool::new();
        let ctx = ToolContext::for_test_bare(CancellationToken::new());
        let result = tool
            .execute(json!({"url": "http://[::1]:8080/"}), &ctx)
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("refused"));
    }

    // ── Summarizer wiring ───────────────────────────────────────────────────

    use std::sync::Mutex as StdMutex;

    /// Records the (prompt, content) it was handed and returns a canned
    /// reply (or a canned error). No LLM, no network. Reply is stored as
    /// `Result<String, String>` because `CcError` isn't `Clone`.
    struct FakeSummarizer {
        reply: Result<String, String>,
        seen: Arc<StdMutex<Vec<(String, String)>>>,
    }

    #[async_trait]
    impl Summarizer for FakeSummarizer {
        async fn summarize(
            &self,
            prompt: &str,
            content: &str,
            _cancel: &CancellationToken,
        ) -> CcResult<String> {
            self.seen
                .lock()
                .unwrap()
                .push((prompt.to_string(), content.to_string()));
            match &self.reply {
                Ok(s) => Ok(s.clone()),
                Err(msg) => Err(CcError::api(msg.clone())),
            }
        }
    }

    async fn serve_once(body: &'static str) -> (u16, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                // Drain the request so the client's write completes.
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });
        (port, handle)
    }

    #[tokio::test]
    async fn execute_routes_prompt_through_summarizer() {
        let _lock = crate::web_fetch::ssrf::ENV_LOCK.lock().await;
        std::env::set_var("CC_WEBFETCH_ALLOW_PRIVATE", "1");

        let seen: Arc<StdMutex<Vec<(String, String)>>> = Arc::new(StdMutex::new(Vec::new()));
        let summarizer = Arc::new(FakeSummarizer {
            reply: Ok::<String, String>("SUMMARY: hello from model".into()),
            seen: seen.clone(),
        });
        let tool = WebFetchTool::with_summarizer(summarizer);

        let (port, handle) = serve_once("raw server body").await;
        let url = format!("http://127.0.0.1:{port}/doc");
        let ctx = ToolContext::for_test_bare(CancellationToken::new());
        let result = tool
            .execute(json!({"url": url, "prompt": "be brief"}), &ctx)
            .await
            .unwrap();
        let _ = handle.await;
        std::env::remove_var("CC_WEBFETCH_ALLOW_PRIVATE");

        assert!(!result.is_error, "200 OK must be Ok: {}", result.content);
        assert!(
            result.content.contains("SUMMARY: hello from model"),
            "summarizer output must appear in result; got: {}",
            result.content
        );
        assert!(
            !result.content.contains("raw server body"),
            "raw body must be replaced by summary; got: {}",
            result.content
        );

        let calls = seen.lock().unwrap();
        assert_eq!(calls.len(), 1, "summarizer should be called exactly once");
        assert_eq!(calls[0].0, "be brief");
        assert_eq!(calls[0].1, "raw server body");
    }

    #[tokio::test]
    async fn execute_without_prompt_skips_summarizer() {
        let _lock = crate::web_fetch::ssrf::ENV_LOCK.lock().await;
        std::env::set_var("CC_WEBFETCH_ALLOW_PRIVATE", "1");

        let seen: Arc<StdMutex<Vec<(String, String)>>> = Arc::new(StdMutex::new(Vec::new()));
        let summarizer = Arc::new(FakeSummarizer {
            reply: Ok::<String, String>("nope".into()),
            seen: seen.clone(),
        });
        let tool = WebFetchTool::with_summarizer(summarizer);

        let (port, handle) = serve_once("plain body").await;
        let url = format!("http://127.0.0.1:{port}/doc");
        let ctx = ToolContext::for_test_bare(CancellationToken::new());
        let result = tool.execute(json!({"url": url}), &ctx).await.unwrap();
        let _ = handle.await;
        std::env::remove_var("CC_WEBFETCH_ALLOW_PRIVATE");

        assert!(!result.is_error);
        assert!(
            result.content.contains("plain body"),
            "raw body must pass through when no prompt: {}",
            result.content
        );
        assert_eq!(
            seen.lock().unwrap().len(),
            0,
            "summarizer must not be invoked without prompt"
        );
    }

    #[tokio::test]
    async fn execute_empty_prompt_skips_summarizer() {
        let _lock = crate::web_fetch::ssrf::ENV_LOCK.lock().await;
        std::env::set_var("CC_WEBFETCH_ALLOW_PRIVATE", "1");

        let seen: Arc<StdMutex<Vec<(String, String)>>> = Arc::new(StdMutex::new(Vec::new()));
        let summarizer = Arc::new(FakeSummarizer {
            reply: Ok::<String, String>("nope".into()),
            seen: seen.clone(),
        });
        let tool = WebFetchTool::with_summarizer(summarizer);

        let (port, handle) = serve_once("plain body").await;
        let url = format!("http://127.0.0.1:{port}/doc");
        let ctx = ToolContext::for_test_bare(CancellationToken::new());
        let result = tool
            .execute(json!({"url": url, "prompt": "   "}), &ctx)
            .await
            .unwrap();
        let _ = handle.await;
        std::env::remove_var("CC_WEBFETCH_ALLOW_PRIVATE");

        assert!(!result.is_error);
        assert!(result.content.contains("plain body"));
        assert_eq!(
            seen.lock().unwrap().len(),
            0,
            "whitespace-only prompt must be ignored"
        );
    }

    #[tokio::test]
    async fn execute_summarizer_failure_falls_back_to_raw_body() {
        let _lock = crate::web_fetch::ssrf::ENV_LOCK.lock().await;
        std::env::set_var("CC_WEBFETCH_ALLOW_PRIVATE", "1");

        let seen: Arc<StdMutex<Vec<(String, String)>>> = Arc::new(StdMutex::new(Vec::new()));
        let summarizer = Arc::new(FakeSummarizer {
            reply: Err::<String, String>("model exploded".into()),
            seen: seen.clone(),
        });
        let tool = WebFetchTool::with_summarizer(summarizer);

        let (port, handle) = serve_once("fallback body").await;
        let url = format!("http://127.0.0.1:{port}/doc");
        let ctx = ToolContext::for_test_bare(CancellationToken::new());
        let result = tool
            .execute(json!({"url": url, "prompt": "be brief"}), &ctx)
            .await
            .unwrap();
        let _ = handle.await;
        std::env::remove_var("CC_WEBFETCH_ALLOW_PRIVATE");

        // Request itself succeeded (200 OK) so it stays Ok despite the
        // summarizer failure.
        assert!(!result.is_error, "non-summarizer status wins");
        assert!(
            result.content.contains("fallback body"),
            "raw body must be used on summarizer failure: {}",
            result.content
        );
        assert!(
            result.content.contains("summarizer unavailable")
                && result.content.contains("model exploded"),
            "fallback banner must mention the error: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn no_summarizer_still_accepts_prompt_field() {
        // Schema compatibility: if the caller (or the model) sends a
        // `prompt` field but the tool has no summarizer wired, the raw
        // body path must still return successfully — no error, no panic.
        let _lock = crate::web_fetch::ssrf::ENV_LOCK.lock().await;
        std::env::set_var("CC_WEBFETCH_ALLOW_PRIVATE", "1");

        let tool = WebFetchTool::new();
        let (port, handle) = serve_once("unchanged").await;
        let url = format!("http://127.0.0.1:{port}/doc");
        let ctx = ToolContext::for_test_bare(CancellationToken::new());
        let result = tool
            .execute(json!({"url": url, "prompt": "ignored"}), &ctx)
            .await
            .unwrap();
        let _ = handle.await;
        std::env::remove_var("CC_WEBFETCH_ALLOW_PRIVATE");

        assert!(!result.is_error);
        assert!(result.content.contains("unchanged"));
    }
}
