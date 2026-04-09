use cc_core::{CcError, CcResult, Message, Usage};
use eventsource_stream::Eventsource;
use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use tokio::sync::mpsc;
use tracing::debug;

use crate::request::CreateMessageRequest;
use crate::stream::{ContentBlockDelta, StreamAccumulator, StreamEvent};

/// Anthropic API base URL.
const ANTHROPIC_API_URL: &str = "https://api.anthropic.com";

/// Anthropic API version header value.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Beta headers for API key auth (extended features).
const ANTHROPIC_BETAS_API_KEY: &str = "interleaved-thinking-2025-05-14";

/// Beta headers for OAuth Bearer token auth.
/// `oauth-2025-04-20` enables Bearer OAuth on public-api routes.
const ANTHROPIC_BETAS_OAUTH: &str =
    "interleaved-thinking-2025-05-14,oauth-2025-04-20";

/// How the client authenticates with the API.
#[derive(Clone)]
pub enum Auth {
    /// Direct API key → `x-api-key: sk-ant-api03-...`
    ApiKey(String),
    /// Claude.ai OAuth token → `Authorization: Bearer sk-ant-oat01-...`
    OAuthToken(String),
}

/// A client for the Anthropic Messages API.
#[derive(Clone)]
pub struct ApiClient {
    http: reqwest::Client,
    auth: Auth,
    base_url: String,
}

impl ApiClient {
    /// Create a client that authenticates with an API key.
    pub fn with_api_key(api_key: impl Into<String>) -> CcResult<Self> {
        Self::new(Auth::ApiKey(api_key.into()))
    }

    /// Create a client that authenticates with an OAuth Bearer token.
    pub fn with_oauth_token(token: impl Into<String>) -> CcResult<Self> {
        Self::new(Auth::OAuthToken(token.into()))
    }

    fn new(auth: Auth) -> CcResult<Self> {
        let http = reqwest::Client::builder()
            .build()
            .map_err(|e| CcError::Api(e.to_string()))?;

        Ok(ApiClient {
            http,
            auth,
            base_url: std::env::var("ANTHROPIC_BASE_URL")
                .unwrap_or_else(|_| ANTHROPIC_API_URL.to_string()),
        })
    }

    fn headers(&self) -> CcResult<HeaderMap> {
        let mut headers = HeaderMap::new();

        match &self.auth {
            Auth::ApiKey(key) => {
                headers.insert(
                    "x-api-key",
                    HeaderValue::from_str(key).map_err(|e| CcError::Auth(e.to_string()))?,
                );
            }
            Auth::OAuthToken(token) => {
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
            Auth::OAuthToken(_) => ANTHROPIC_BETAS_OAUTH,
            Auth::ApiKey(_) => ANTHROPIC_BETAS_API_KEY,
        };
        headers.insert(
            "anthropic-beta",
            HeaderValue::from_static(betas),
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        // Identify as the Claude Code CLI (matches TS version's x-app header).
        headers.insert("x-app", HeaderValue::from_static("cli"));
        Ok(headers)
    }

    /// Stream a message request, yielding `StreamEvent`s via a channel.
    pub async fn stream_message(
        &self,
        request: CreateMessageRequest,
    ) -> CcResult<mpsc::Receiver<CcResult<StreamEvent>>> {
        let url = format!("{}/v1/messages", self.base_url);
        let headers = self.headers()?;

        let body = serde_json::to_value(&request).map_err(CcError::Json)?;

        debug!(model = %request.model, "starting streaming request");

        let response = self
            .http
            .post(&url)
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|e| CcError::Api(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response
                .text()
                .await
                .unwrap_or_else(|_| "<unreadable body>".into());
            return Err(CcError::Api(format!("HTTP {status}: {text}")));
        }

        let (tx, rx) = mpsc::channel::<CcResult<StreamEvent>>(64);

        let byte_stream = response.bytes_stream();
        tokio::spawn(async move {
            let mut sse = byte_stream.eventsource();
            while let Some(item) = sse.next().await {
                match item {
                    Ok(event) => {
                        if event.event == "ping"
                            || event.data.is_empty()
                            || event.data == "[DONE]"
                        {
                            continue;
                        }
                        // The API may emit an error event in-band (e.g. overloaded_error
                        // mid-stream) with a 200 HTTP status. Surface it as a hard error
                        // instead of letting the StreamEvent parse silently fall into the
                        // `Unknown` catch-all and the stream terminate cleanly — that made
                        // `--print` return empty output with exit=0, hiding real failures.
                        //
                        // Detect by the SSE `event: error` name *or* by the JSON payload
                        // having `"type":"error"` at the top level, since
                        // `eventsource_stream` does not always populate `event.event`.
                        if event.event == "error" || is_error_payload(&event.data) {
                            let msg = parse_in_stream_error(&event.data)
                                .unwrap_or_else(|| event.data.clone());
                            let _ = tx
                                .send(Err(CcError::Api(format!("stream error: {msg}"))))
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
                        let _ = tx.send(Err(CcError::Api(e.to_string()))).await;
                        break;
                    }
                }
            }
        });

        Ok(rx)
    }

    /// Stream and accumulate a complete message.
    /// Calls `on_text` for each text delta (for real-time display).
    pub async fn complete_message<F>(
        &self,
        request: CreateMessageRequest,
        mut on_text: F,
    ) -> CcResult<Message>
    where
        F: FnMut(&str),
    {
        let mut rx = self.stream_message(request).await?;
        let mut acc = StreamAccumulator::default();

        while let Some(event) = rx.recv().await {
            let event = event?;
            if let StreamEvent::ContentBlockDelta {
                delta: ContentBlockDelta::TextDelta { text },
                ..
            } = &event
            {
                on_text(text);
            }
            acc.apply(&event);
        }

        let id = acc.message_id.clone();
        let model = acc.model.clone();
        let stop_reason = acc.stop_reason.clone();
        let input_tokens = acc.input_tokens;
        let output_tokens = acc.output_tokens;
        let content = acc.into_content();

        Ok(Message {
            id,
            kind: "message".into(),
            role: cc_core::Role::Assistant,
            content,
            model,
            stop_reason,
            stop_sequence: None,
            usage: Usage {
                input_tokens,
                output_tokens,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            },
        })
    }
}

/// Check whether an SSE `data:` payload is an error envelope.
/// Matches `{"type":"error", ...}` at the top level.
fn is_error_payload(data: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
        return false;
    };
    value.get("type").and_then(|v| v.as_str()) == Some("error")
}

/// Extract a human-readable message from an in-stream `event: error` payload.
/// The Anthropic API uses two shapes depending on the failure:
///   - `{"type":"error","error":{"type":"...","message":"..."}}`
///   - `{"error":{"type":"...","message":"..."}}`
///
/// Returns `Some("<type>: <message>")` on either, or `None` if it can't parse.
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
}
