use cc_core::{CcError, CcResult, Message, Usage};
use eventsource_stream::Eventsource;
use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use thiserror::Error;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::request::CreateMessageRequest;
use crate::retry::RetryPolicy;
use crate::stream::{ContentBlockDelta, StreamAccumulator, StreamError, StreamEvent};

/// Errors returned by [`ApiClient::complete_message`].
///
/// Split into `Core` (HTTP / IO / auth / parse-of-envelope) and `Stream`
/// (semantic problems in the accumulated SSE payload, such as a `tool_use`
/// block whose JSON buffer is not valid JSON at end-of-stream). Keeping
/// `Stream` structured lets `cc-query` translate it into a synthetic
/// `tool_result` with `is_error: true` instead of forwarding silent garbage
/// to the model.
#[derive(Debug, Error)]
pub enum ApiError {
    #[error(transparent)]
    Core(#[from] CcError),

    #[error(transparent)]
    Stream(#[from] StreamError),
}

impl From<ApiError> for CcError {
    fn from(e: ApiError) -> Self {
        match e {
            ApiError::Core(c) => c,
            ApiError::Stream(s) => s.into(),
        }
    }
}

/// Anthropic API base URL.
const ANTHROPIC_API_URL: &str = "https://api.anthropic.com";

/// Anthropic API version header value.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Beta headers for API key auth (extended features).
const ANTHROPIC_BETAS_API_KEY: &str = "interleaved-thinking-2025-05-14";

/// Beta headers for OAuth Bearer token auth.
const ANTHROPIC_BETAS_OAUTH: &str = "interleaved-thinking-2025-05-14,oauth-2025-04-20";

/// How the client authenticates with the API.
#[derive(Clone)]
pub enum AuthCredential {
    /// Direct API key -> `x-api-key: sk-ant-api03-...`
    ApiKey(String),
    /// Claude.ai OAuth token -> `Authorization: Bearer sk-ant-oat01-...`
    OAuthToken(String),
}

/// Deltas emitted during streaming (for TUI display).
#[derive(Debug, Clone)]
pub enum StreamDelta {
    Text(String),
    Thinking(String),
    ToolUseStart { id: String, name: String },
    InputJsonDelta(String),
}

/// A client for the Anthropic Messages API.
#[derive(Clone)]
pub struct ApiClient {
    http: reqwest::Client,
    auth: AuthCredential,
    base_url: String,
    retry: RetryPolicy,
}

impl ApiClient {
    /// Create a client using a pre-built reqwest::Client from cc-http.
    pub fn new(http: reqwest::Client, auth: AuthCredential) -> Self {
        let base_url =
            std::env::var("ANTHROPIC_BASE_URL").unwrap_or_else(|_| ANTHROPIC_API_URL.to_string());
        Self {
            http,
            auth,
            base_url,
            retry: RetryPolicy::default(),
        }
    }

    /// Set a custom retry policy.
    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    fn headers(&self) -> CcResult<HeaderMap> {
        let mut headers = HeaderMap::new();

        match &self.auth {
            AuthCredential::ApiKey(key) => {
                headers.insert(
                    "x-api-key",
                    HeaderValue::from_str(key).map_err(|e| CcError::Auth(e.to_string()))?,
                );
            }
            AuthCredential::OAuthToken(token) => {
                let bearer = format!("Bearer {token}");
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&bearer).map_err(|e| CcError::Auth(e.to_string()))?,
                );
            }
        }

        headers.insert(
            "anthropic-version",
            HeaderValue::from_static(ANTHROPIC_VERSION),
        );
        let betas = match &self.auth {
            AuthCredential::OAuthToken(_) => ANTHROPIC_BETAS_OAUTH,
            AuthCredential::ApiKey(_) => ANTHROPIC_BETAS_API_KEY,
        };
        headers.insert("anthropic-beta", HeaderValue::from_static(betas));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert("x-app", HeaderValue::from_static("cli"));
        Ok(headers)
    }

    /// Send the HTTP request with retry on transient failures.
    /// Returns the successful response or the last error.
    async fn send_with_retry(
        &self,
        request: &CreateMessageRequest,
        cancel: &CancellationToken,
    ) -> CcResult<reqwest::Response> {
        let url = format!("{}/v1/messages", self.base_url);
        let headers = self.headers()?;
        let body = serde_json::to_value(request).map_err(CcError::Json)?;

        let mut attempt = 0u32;
        loop {
            debug!(model = %request.model, attempt, "sending streaming request");

            let response = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(CcError::Cancelled),
                result = self.http.post(&url).headers(headers.clone()).json(&body).send() => {
                    result.map_err(|e| CcError::api(e.to_string()))?
                }
            };

            if response.status().is_success() {
                return Ok(response);
            }

            let status = response.status();
            let status_code = status.as_u16();

            // Parse Retry-After header before consuming the body
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());

            let text = response
                .text()
                .await
                .unwrap_or_else(|_| "<unreadable body>".into());

            let err = if status_code == 429 {
                CcError::RateLimited { retry_after }
            } else if status_code >= 500 {
                CcError::api_retryable(format!("HTTP {status}: {text}"), status_code)
            } else {
                return Err(CcError::api(format!("HTTP {status}: {text}")));
            };

            // Check retry policy
            if let Some(delay) = self.retry.should_retry(&err, attempt) {
                warn!(
                    attempt,
                    delay_ms = delay.as_millis() as u64,
                    "retrying after transient error"
                );
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err(CcError::Cancelled),
                    _ = tokio::time::sleep(delay) => {},
                }
                attempt += 1;
            } else {
                return Err(err);
            }
        }
    }

    /// Stream a message request, yielding `StreamEvent`s via a channel.
    /// Retries transient errors (429/5xx) per the retry policy.
    /// Cancellable via the CancellationToken.
    pub async fn stream_message(
        &self,
        request: CreateMessageRequest,
        cancel: &CancellationToken,
    ) -> CcResult<mpsc::Receiver<CcResult<StreamEvent>>> {
        let response = self.send_with_retry(&request, cancel).await?;

        let (tx, rx) = mpsc::channel::<CcResult<StreamEvent>>(64);
        let cancel = cancel.clone();

        let byte_stream = response.bytes_stream();
        tokio::spawn(async move {
            let mut sse = byte_stream.eventsource();
            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break,
                    item = sse.next() => {
                        let Some(item) = item else { break };
                        match item {
                            Ok(event) => {
                                if event.event == "ping"
                                    || event.data.is_empty()
                                    || event.data == "[DONE]"
                                {
                                    continue;
                                }
                                if event.event == "error" || is_error_payload(&event.data) {
                                    let msg = parse_in_stream_error(&event.data)
                                        .unwrap_or_else(|| event.data.clone());
                                    let _ = tx
                                        .send(Err(CcError::api(format!("stream error: {msg}"))))
                                        .await;
                                    break;
                                }
                                match serde_json::from_str::<StreamEvent>(&event.data) {
                                    Ok(se) => {
                                        if tx.send(Ok(se)).await.is_err() {
                                            break;
                                        }
                                    }
                                    Err(e) => {
                                        debug!("SSE parse error: {e}  data={}", event.data);
                                    }
                                }
                            }
                            Err(e) => {
                                let _ = tx.send(Err(CcError::api(e.to_string()))).await;
                                break;
                            }
                        }
                    }
                }
            }
        });

        Ok(rx)
    }

    /// Stream and accumulate a complete message.
    ///
    /// Calls `on_delta` for each streaming delta (text / thinking / tool-use
    /// start / input JSON delta) so the TUI can paint tokens as they arrive.
    ///
    /// Returns the fully-assembled `Message` + `Usage`. When the accumulated
    /// payload is structurally invalid at end-of-stream (e.g. a `tool_use`
    /// block whose JSON buffer never parses — the H1 fix behind
    /// `StreamError::ToolInputNotJson`) the error is surfaced as
    /// `CcError::Api`; callers can translate it into a synthetic
    /// `tool_result` with `is_error: true` so the model retries with
    /// well-formed arguments.
    pub async fn complete_message(
        &self,
        request: CreateMessageRequest,
        mut on_delta: impl FnMut(StreamDelta),
        cancel: &CancellationToken,
    ) -> CcResult<(Message, Usage)> {
        let mut rx = self.stream_message(request, cancel).await?;
        let mut acc = StreamAccumulator::default();

        while let Some(event) = rx.recv().await {
            let event = event?;
            // Emit deltas for TUI
            match &event {
                StreamEvent::ContentBlockDelta {
                    delta: ContentBlockDelta::TextDelta { text },
                    ..
                } => on_delta(StreamDelta::Text(text.clone())),
                StreamEvent::ContentBlockDelta {
                    delta: ContentBlockDelta::ThinkingDelta { thinking },
                    ..
                } => on_delta(StreamDelta::Thinking(thinking.clone())),
                StreamEvent::ContentBlockDelta {
                    delta: ContentBlockDelta::InputJsonDelta { partial_json },
                    ..
                } => on_delta(StreamDelta::InputJsonDelta(partial_json.clone())),
                StreamEvent::ContentBlockStart {
                    content_block: crate::stream::ContentBlockStartData::ToolUse { id, name, .. },
                    ..
                } => on_delta(StreamDelta::ToolUseStart {
                    id: id.clone(),
                    name: name.clone(),
                }),
                _ => {}
            }
            acc.apply(&event);
        }

        let id = acc.message_id.clone();
        let model = acc.model.clone();
        let stop_reason = acc.stop_reason;
        let usage = Usage {
            input_tokens: acc.input_tokens,
            output_tokens: acc.output_tokens,
            cache_creation_input_tokens: acc.cache_creation_input_tokens,
            cache_read_input_tokens: acc.cache_read_input_tokens,
        };
        // `into_content` is fallible after the H1 fix — a `tool_use` block
        // whose JSON never parsed returns `StreamError::ToolInputNotJson`.
        // Convert to `CcError::Api` so callers get the raw context.
        let content = acc
            .into_content()
            .map_err(|e| CcError::api(format!("stream accumulator: {e}")))?;

        let message = Message {
            id,
            kind: "message".into(),
            role: cc_core::Role::Assistant,
            content,
            model,
            stop_reason,
            stop_sequence: None,
            usage: usage.clone(),
        };

        Ok((message, usage))
    }
}

/// Check whether an SSE `data:` payload is an error envelope.
fn is_error_payload(data: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
        return false;
    };
    value.get("type").and_then(|v| v.as_str()) == Some("error")
}

/// Extract a human-readable message from an in-stream error payload.
fn parse_in_stream_error(data: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(data).ok()?;
    let err = value.get("error")?;
    let kind = err.get("type").and_then(|v| v.as_str()).unwrap_or("error");
    let msg = err
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("(no message)");
    Some(format!("{kind}: {msg}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_stream_error_wrapped_type_field() {
        let data = r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
        assert_eq!(
            parse_in_stream_error(data).as_deref(),
            Some("overloaded_error: Overloaded")
        );
    }

    #[test]
    fn in_stream_error_bare_error_field() {
        let data = r#"{"error":{"type":"rate_limit","message":"slow down"}}"#;
        assert_eq!(
            parse_in_stream_error(data).as_deref(),
            Some("rate_limit: slow down")
        );
    }

    #[test]
    fn in_stream_error_malformed_returns_none() {
        assert_eq!(parse_in_stream_error("not json"), None);
        assert_eq!(parse_in_stream_error(r#"{"foo":1}"#), None);
    }

    #[test]
    fn is_error_payload_detects_top_level_type() {
        assert!(is_error_payload(
            r#"{"type":"error","error":{"type":"overloaded_error","message":"x"}}"#
        ));
        assert!(!is_error_payload(
            r#"{"type":"message_start","message":{}}"#
        ));
        assert!(!is_error_payload("not json"));
    }

    #[tokio::test]
    async fn pre_cancelled_token_short_circuits_before_network() {
        // Pre-cancel the token; stream_message should return Cancelled without
        // attempting to hit the network. The `biased;` in send_with_retry
        // guarantees the cancel branch fires before the .send() future even
        // starts driving. We point base_url at a hostname that will take
        // seconds to fail DNS lookup, so if cancel were ignored the test
        // would either time out or fail after a real delay.
        let http = reqwest::Client::new();
        let client = ApiClient {
            http,
            auth: AuthCredential::ApiKey("sk-test".into()),
            base_url: "http://nonexistent.invalid.claudecode.test:1".into(),
            retry: RetryPolicy::default(),
        };
        let cancel = CancellationToken::new();
        cancel.cancel();

        let req = CreateMessageRequest::new("claude-opus-4-7", vec![]);

        let start = std::time::Instant::now();
        let result = client.stream_message(req, &cancel).await;
        let elapsed = start.elapsed();

        assert!(
            matches!(result, Err(CcError::Cancelled)),
            "got {:?}",
            result.err()
        );
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "took {:?}",
            elapsed
        );
    }
}
