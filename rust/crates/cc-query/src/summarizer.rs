//! `ApiSummarizer` — concrete `cc_core::Summarizer` backed by `cc_api::ApiClient`.
//!
//! Wired from `main.rs` into `cc_tools::web_fetch::WebFetchTool` so the tool
//! can honour its `prompt` field by delegating a single-turn model call.
//!
//! Why this lives in `cc-query`: it's the first crate above `cc-tools` that
//! already depends on `cc-api`. Putting the impl here avoids adding an
//! `cc-api` dep to `cc-tools` (which must stay provider-agnostic — the
//! same rule that motivated `SubAgentRunner`).

use async_trait::async_trait;
use cc_api::{ApiClient, CreateMessageRequest, StreamDelta};
use cc_core::{CcError, CcResult, ContentBlock, MessageContent, MessageParam, Role, Summarizer};
use tokio_util::sync::CancellationToken;

/// A `Summarizer` that runs a single no-tool, no-system-prompt turn
/// against the same model the user is chatting with.
pub struct ApiSummarizer {
    api: ApiClient,
    model: String,
    max_tokens: u32,
}

impl ApiSummarizer {
    pub fn new(api: ApiClient, model: impl Into<String>) -> Self {
        Self {
            api,
            model: model.into(),
            max_tokens: 1024,
        }
    }

    /// Override the max output tokens (default 1024, matching a typical
    /// short summary budget).
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }
}

#[async_trait]
impl Summarizer for ApiSummarizer {
    async fn summarize(
        &self,
        prompt: &str,
        content: &str,
        cancel: &CancellationToken,
    ) -> CcResult<String> {
        // Single user turn: prompt + content, clearly delimited.
        let user_text =
            format!("{prompt}\n\n--- BEGIN CONTENT ---\n{content}\n--- END CONTENT ---");
        let user_msg = MessageParam {
            role: Role::User,
            content: MessageContent::Text(user_text),
        };

        let req =
            CreateMessageRequest::new(&self.model, vec![user_msg]).with_max_tokens(self.max_tokens);

        // We don't need per-delta callbacks — just the final text. The
        // callback therefore swallows deltas; complete_message returns the
        // assembled Message + Usage.
        let (message, _usage) = self
            .api
            .complete_message(req, |_: StreamDelta| {}, cancel)
            .await?;

        let text: String = message
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");

        if text.is_empty() {
            return Err(CcError::api("summarizer returned empty response"));
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_summarizer_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ApiSummarizer>();
    }
}
