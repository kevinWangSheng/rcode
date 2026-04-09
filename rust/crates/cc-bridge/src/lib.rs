//! cc-bridge — non-interactive SDK-style entry point for `claude --print`.
//!
//! This crate is the seam between "everything assembled in `cc/main.rs`" and a
//! callable function that runs one query end-to-end. It exists so external SDKs
//! and the `--print` flag can share the same path: build a `BridgeRequest`,
//! call [`run_once`], get a [`BridgeResponse`].
//!
//! Unlike the TUI path, this crate intentionally never prompts for permission
//! interactively — it uses the engine's `non_interactive` mode (auto-deny) so
//! it can run in a pipeline. Callers that need approve-by-default can flip
//! `bypass_permissions` instead.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use cc_api::ApiClient;
use cc_core::{MessageParam, SystemBlock};
use cc_hooks::HookRunner;
use cc_permissions::PermissionEngine;
use cc_query::{QueryEngine, QueryOptions};
use cc_session::Session;
use cc_tools::Tool;

/// One-shot request — everything the bridge needs to run a single turn.
pub struct BridgeRequest {
    pub api: ApiClient,
    pub tools: Vec<Arc<dyn Tool>>,
    pub permissions: PermissionEngine,
    pub hooks: HookRunner,
    pub session: Session,
    pub system_blocks: Vec<SystemBlock>,
    pub initial_messages: Vec<MessageParam>,
    pub user_text: String,
    pub model: String,
    pub max_tokens: u32,
    /// When true, permission `Ask` rules auto-deny instead of prompting.
    pub non_interactive: bool,
    /// When true, all permission checks are skipped.
    pub bypass_permissions: bool,
}

/// Response from a single bridge run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeResponse {
    pub session_id: String,
    pub content: String,
}

/// Errors surfaced by the bridge. We deliberately collapse engine internals to
/// `Engine(String)` so callers don't depend on cc-query internals.
#[derive(Debug, Error)]
pub enum BridgeError {
    #[error("engine error: {0}")]
    Engine(String),
}

/// Run a single conversation turn and return the response. Streams text via the
/// `on_text` callback so callers can render progress (e.g. write to stdout
/// token-by-token in `--print` mode). After this returns the engine is dropped.
pub async fn run_once<F>(req: BridgeRequest, mut on_text: F) -> Result<BridgeResponse, BridgeError>
where
    F: FnMut(&str),
{
    let BridgeRequest {
        api,
        tools,
        permissions,
        hooks,
        session,
        system_blocks,
        mut initial_messages,
        user_text,
        model,
        max_tokens,
        non_interactive,
        bypass_permissions,
    } = req;

    let options = QueryOptions {
        model,
        max_tokens,
        non_interactive,
        bypass_permissions,
    };

    let mut engine = QueryEngine::new(
        api,
        tools,
        permissions,
        hooks,
        session,
        system_blocks,
        options,
    );

    let session_id = engine.session().id.clone();

    let content = engine
        .run_turn(user_text, |delta| on_text(delta), &mut initial_messages)
        .await
        .map_err(|e| BridgeError::Engine(e.to_string()))?;

    Ok(BridgeResponse { session_id, content })
}
