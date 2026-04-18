//! Summarizer trait — lets tools (today only `WebFetch`) delegate small
//! one-shot LLM calls back to the `cc-api` layer without introducing a
//! circular dependency.
//!
//! Why this lives in `cc-core`: `cc-tools` depends on `cc-core` but must
//! not depend on `cc-api` (the tools layer is deliberately provider-agnostic
//! so it can be reused by subagents, the bridge, and tests). The concrete
//! impl lives in `cc-query::summarizer::ApiSummarizer`, and `main.rs`
//! injects it at startup — same escape hatch as `SubAgentRunner`.
//!
//! Scope: small single-turn calls. No tool use, no streaming through the
//! TUI, no session append. If you need more, build a proper sub-agent.

use crate::CcResult;
use tokio_util::sync::CancellationToken;

/// A tiny single-turn LLM call used by tools that want to transform a
/// fetched body into a focused answer.
///
/// `prompt` is the user-supplied instruction (e.g. the `prompt` field of
/// the WebFetch tool). `content` is the raw material being summarized.
/// Implementations should combine them into a single user message and
/// call the API without tools or system prompts so the behaviour is
/// deterministic per prompt/content pair.
#[async_trait::async_trait]
pub trait Summarizer: Send + Sync {
    async fn summarize(
        &self,
        prompt: &str,
        content: &str,
        cancel: &CancellationToken,
    ) -> CcResult<String>;
}
