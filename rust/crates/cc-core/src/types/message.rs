use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Role in a conversation turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// A single cache control directive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheControl {
    #[serde(rename = "type")]
    pub kind: String, // "ephemeral"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>, // "global" | "org"
}

impl CacheControl {
    /// Ephemeral cache entry with `scope = "global"` — use for the static
    /// instruction prompt (tier 2 of the three-tier cache scheme).
    pub fn ephemeral_global() -> Self {
        CacheControl {
            kind: "ephemeral".into(),
            scope: Some("global".into()),
        }
    }

    /// Ephemeral cache entry with `scope = "org"` — use for dynamic / per-org
    /// system blocks such as git context and memory (tier 3).
    pub fn ephemeral_org() -> Self {
        CacheControl {
            kind: "ephemeral".into(),
            scope: Some("org".into()),
        }
    }

    /// Ephemeral cache entry with no scope. The server treats it as an
    /// unscoped ephemeral cache request; prefer `ephemeral_global` /
    /// `ephemeral_org` when the three-tier contract applies.
    pub fn ephemeral_unscoped() -> Self {
        CacheControl {
            kind: "ephemeral".into(),
            scope: None,
        }
    }
}

/// A text content block.
/// Note: no `kind`/`type` field — the `ContentBlock` enum tag handles "type" for serde.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextBlock {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

/// A tool-use content block (model requests a tool call).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolUseBlock {
    pub id: String,
    pub name: String,
    pub input: Value,
}

/// A tool-result content block (user provides the result of a tool call).
/// Uses `#[serde(untagged)]` path — `ContentBlock::ToolResult` adds the "type" tag.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultBlock {
    pub tool_use_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

/// A content block in an Anthropic message.
/// `#[serde(tag = "type", rename_all = "snake_case")]` injects `"type": "text"` /
/// `"type": "tool_use"` / `"type": "tool_result"` during (de)serialization.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text(TextBlock),
    ToolUse(ToolUseBlock),
    ToolResult(ToolResultBlock),
}

impl ContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        ContentBlock::Text(TextBlock {
            text: text.into(),
            cache_control: None,
        })
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            ContentBlock::Text(b) => Some(&b.text),
            _ => None,
        }
    }
}

/// A system prompt block with optional cache control.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemBlock {
    #[serde(rename = "type")]
    pub kind: String, // "text"
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

/// A message in the conversation (user or assistant turn).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageParam {
    pub role: Role,
    pub content: MessageContent,
}

/// Message content — either a plain string or structured blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

impl MessageParam {
    pub fn user(text: impl Into<String>) -> Self {
        MessageParam {
            role: Role::User,
            content: MessageContent::Text(text.into()),
        }
    }

    pub fn user_blocks(blocks: Vec<ContentBlock>) -> Self {
        MessageParam {
            role: Role::User,
            content: MessageContent::Blocks(blocks),
        }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        MessageParam {
            role: Role::Assistant,
            content: MessageContent::Text(text.into()),
        }
    }

    pub fn assistant_blocks(blocks: Vec<ContentBlock>) -> Self {
        MessageParam {
            role: Role::Assistant,
            content: MessageContent::Blocks(blocks),
        }
    }
}

/// Usage statistics returned by the API.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u32>,
}

/// The reason the model stopped generating.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    StopSequence,
    ToolUse,
}

/// A completed message response from the API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub role: Role,
    pub content: Vec<ContentBlock>,
    pub model: String,
    pub stop_reason: Option<StopReason>,
    pub stop_sequence: Option<String>,
    pub usage: Usage,
}

impl Message {
    /// Return the concatenated text from all text blocks.
    pub fn text_content(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| b.as_text())
            .collect::<Vec<_>>()
            .join("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ephemeral_global_has_expected_kind_and_scope() {
        let cc = CacheControl::ephemeral_global();
        assert_eq!(cc.kind, "ephemeral");
        assert_eq!(cc.scope.as_deref(), Some("global"));
    }

    #[test]
    fn ephemeral_org_has_expected_kind_and_scope() {
        let cc = CacheControl::ephemeral_org();
        assert_eq!(cc.kind, "ephemeral");
        assert_eq!(cc.scope.as_deref(), Some("org"));
    }

    #[test]
    fn ephemeral_unscoped_has_expected_kind_and_no_scope() {
        let cc = CacheControl::ephemeral_unscoped();
        assert_eq!(cc.kind, "ephemeral");
        assert!(cc.scope.is_none());
    }

    #[test]
    fn cache_control_wire_shape_global() {
        let cc = CacheControl::ephemeral_global();
        let json = serde_json::to_value(&cc).expect("serialize");
        assert_eq!(
            json,
            serde_json::json!({"type": "ephemeral", "scope": "global"})
        );
    }

    #[test]
    fn cache_control_wire_shape_org() {
        let cc = CacheControl::ephemeral_org();
        let json = serde_json::to_value(&cc).expect("serialize");
        assert_eq!(
            json,
            serde_json::json!({"type": "ephemeral", "scope": "org"})
        );
    }

    #[test]
    fn cache_control_wire_shape_unscoped_omits_scope() {
        let cc = CacheControl::ephemeral_unscoped();
        let json = serde_json::to_value(&cc).expect("serialize");
        assert_eq!(json, serde_json::json!({"type": "ephemeral"}));
    }
}
