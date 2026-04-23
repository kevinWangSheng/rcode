use cc_core::{MessageParam, SystemBlock, ThinkingConfig, ToolDefinition};
use serde::{Deserialize, Serialize};

/// Request body for `POST /v1/messages` (streaming).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateMessageRequest {
    pub model: String,
    pub max_tokens: u32,
    pub messages: Vec<MessageParam>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<Vec<SystemBlock>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDefinition>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,

    /// Extended-thinking config. `None` omits the field (matches TS which
    /// only includes `thinking` when the caller opted in — see
    /// `src/utils/sideQuery.ts:169-193`). Serializes as
    /// `{"type":"enabled","budget_tokens":N}` / `{"type":"disabled"}` /
    /// `{"type":"adaptive"}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingConfig>,

    /// Must be `true` for streaming requests.
    pub stream: bool,
}

impl CreateMessageRequest {
    /// Create a minimal streaming request.
    pub fn new(model: impl Into<String>, messages: Vec<MessageParam>) -> Self {
        CreateMessageRequest {
            model: model.into(),
            max_tokens: 8192,
            messages,
            system: None,
            tools: None,
            temperature: None,
            stop_sequences: None,
            thinking: None,
            stream: true,
        }
    }

    pub fn with_system(mut self, blocks: Vec<SystemBlock>) -> Self {
        self.system = Some(blocks);
        self
    }

    pub fn with_tools(mut self, tools: Vec<ToolDefinition>) -> Self {
        self.tools = Some(tools);
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Attach an extended-thinking config. Use
    /// [`ThinkingConfig::enabled`] for an explicit budget; callers MUST
    /// ensure `budget_tokens < max_tokens`. Mirrors the TS
    /// `sideQuery.ts:172-177` clamp.
    #[must_use]
    pub fn with_thinking(mut self, thinking: ThinkingConfig) -> Self {
        self.thinking = Some(thinking);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cc_core::{CacheControl, ContentBlock, MessageParam, Role};

    #[test]
    fn default_request_omits_thinking_on_wire() {
        // Default serialization must not emit `thinking` — otherwise
        // models that don't support extended thinking would 400.
        let req = CreateMessageRequest::new("claude-opus-4-7", vec![]);
        let json = serde_json::to_value(&req).unwrap();
        assert!(
            json.get("thinking").is_none(),
            "thinking must be absent when unset"
        );
    }

    #[test]
    fn with_thinking_emits_enabled_shape_on_wire() {
        let req = CreateMessageRequest::new("claude-opus-4-7", vec![])
            .with_max_tokens(8192)
            .with_thinking(ThinkingConfig::enabled(2048));
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(
            json["thinking"],
            serde_json::json!({"type": "enabled", "budget_tokens": 2048})
        );
    }

    #[test]
    fn with_thinking_emits_disabled_shape_on_wire() {
        let req = CreateMessageRequest::new("claude-opus-4-7", vec![])
            .with_thinking(ThinkingConfig::Disabled);
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["thinking"], serde_json::json!({"type": "disabled"}));
    }

    #[test]
    fn last_content_block_cache_control_reaches_wire() {
        // Ground-truth wire shape for P0 #1: when a caller tags the last
        // content block with `cache_control`, the serialized request body
        // MUST carry `{"type":"ephemeral"}` on that block. This is the
        // gap that silently killed prompt-caching before this change.
        let user_msg = MessageParam {
            role: Role::User,
            content: cc_core::MessageContent::Blocks(vec![
                ContentBlock::text("context"),
                ContentBlock::text("latest")
                    .with_cache_control(CacheControl::ephemeral_unscoped()),
            ]),
        };
        let req = CreateMessageRequest::new("claude-opus-4-7", vec![user_msg]);
        let json = serde_json::to_value(&req).unwrap();
        let blocks = &json["messages"][0]["content"];
        assert!(
            blocks[0].get("cache_control").is_none(),
            "non-trailing blocks must not carry cache_control"
        );
        assert_eq!(
            blocks[1]["cache_control"],
            serde_json::json!({"type": "ephemeral"}),
            "trailing block's cache_control must reach the wire"
        );
    }

    #[test]
    fn tool_result_cache_control_reaches_wire() {
        // Same wire-shape guarantee but for a tool_result-terminated
        // message (the common case in agentic turns).
        let msg = MessageParam {
            role: Role::User,
            content: cc_core::MessageContent::Blocks(vec![ContentBlock::ToolResult(
                cc_core::ToolResultBlock {
                    tool_use_id: "tu1".into(),
                    content: Some(serde_json::Value::String("ok".into())),
                    is_error: None,
                    cache_control: Some(CacheControl::ephemeral_unscoped()),
                },
            )]),
        };
        let req = CreateMessageRequest::new("claude-opus-4-7", vec![msg]);
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(
            json["messages"][0]["content"][0]["cache_control"],
            serde_json::json!({"type": "ephemeral"})
        );
    }
}
