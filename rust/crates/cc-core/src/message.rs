use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── Roles & Cache ──────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheControl {
    #[serde(rename = "type")]
    pub kind: String, // "ephemeral"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>, // "global" | "org"
}

impl CacheControl {
    /// Ephemeral block with the `global` cache scope. Use for static prompt
    /// blocks (e.g. the main instruction / tool-description block) that are
    /// bit-identical across every invocation of the CLI. Global is the
    /// longest-lived tier.
    pub fn ephemeral_global() -> Self {
        Self {
            kind: "ephemeral".into(),
            scope: Some("global".into()),
        }
    }

    /// Ephemeral block with the `org` cache scope. Use for per-session
    /// dynamic blocks (git context, memory files, per-project settings)
    /// that vary across users but stay stable within one org / session.
    pub fn ephemeral_org() -> Self {
        Self {
            kind: "ephemeral".into(),
            scope: Some("org".into()),
        }
    }

    /// Ephemeral block with no explicit scope — the server falls back to
    /// its default (effectively session-local) tier. Provided for parity
    /// with the wire format; new call sites should prefer `ephemeral_global`
    /// or `ephemeral_org`.
    pub fn ephemeral_unscoped() -> Self {
        Self {
            kind: "ephemeral".into(),
            scope: None,
        }
    }
}

#[cfg(test)]
mod cache_control_tests {
    use super::CacheControl;

    #[test]
    fn ephemeral_global_sets_scope_global() {
        let c = CacheControl::ephemeral_global();
        assert_eq!(c.kind, "ephemeral");
        assert_eq!(c.scope.as_deref(), Some("global"));
    }

    #[test]
    fn ephemeral_org_sets_scope_org() {
        let c = CacheControl::ephemeral_org();
        assert_eq!(c.kind, "ephemeral");
        assert_eq!(c.scope.as_deref(), Some("org"));
    }

    #[test]
    fn ephemeral_unscoped_leaves_scope_none() {
        let c = CacheControl::ephemeral_unscoped();
        assert_eq!(c.kind, "ephemeral");
        assert_eq!(c.scope, None);
    }

    #[test]
    fn serializes_without_scope_when_unscoped() {
        let c = CacheControl::ephemeral_unscoped();
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(json, r#"{"type":"ephemeral"}"#);
    }

    #[test]
    fn serializes_with_scope_when_global() {
        let c = CacheControl::ephemeral_global();
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(json, r#"{"type":"ephemeral","scope":"global"}"#);
    }

    #[test]
    fn serializes_with_scope_when_org() {
        let c = CacheControl::ephemeral_org();
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(json, r#"{"type":"ephemeral","scope":"org"}"#);
    }
}

// ── Content Blocks ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextBlock {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolUseBlock {
    pub id: String,
    pub name: String,
    pub input: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultBlock {
    pub tool_use_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingBlock {
    pub thinking: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedactedThinkingBlock {
    pub data: String, // opaque base64 blob
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageBlock {
    pub source: ImageSource,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ImageSource {
    #[serde(rename = "base64")]
    Base64 { media_type: String, data: String },
    #[serde(rename = "url")]
    Url { url: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text(TextBlock),
    ToolUse(ToolUseBlock),
    ToolResult(ToolResultBlock),
    Thinking(ThinkingBlock),
    RedactedThinking(RedactedThinkingBlock),
    Image(ImageBlock),
    /// Forward-compat: unknown block types preserved as raw JSON.
    #[serde(other)]
    Unknown,
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

// ── System Blocks ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemBlock {
    #[serde(rename = "type")]
    pub kind: String, // "text"
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

// ── Conversation Messages ──────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageParam {
    pub role: Role,
    pub content: MessageContent,
}

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

// ── API Response ───────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    StopSequence,
    ToolUse,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String, // "message"
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
    fn role_serialization() {
        assert_eq!(serde_json::to_string(&Role::User).unwrap(), "\"user\"");
        assert_eq!(
            serde_json::to_string(&Role::Assistant).unwrap(),
            "\"assistant\""
        );
    }

    #[test]
    fn content_block_text_round_trip() {
        let block = ContentBlock::text("hello");
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("\"type\":\"text\""));
        assert!(json.contains("\"text\":\"hello\""));
        let parsed: ContentBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.as_text(), Some("hello"));
    }

    #[test]
    fn unknown_block_type_preserved() {
        let json = r#"{"type":"future_type","data":"something"}"#;
        let block: ContentBlock = serde_json::from_str(json).unwrap();
        assert!(matches!(block, ContentBlock::Unknown));
    }

    #[test]
    fn message_content_untagged() {
        let text: MessageContent = serde_json::from_str("\"hello\"").unwrap();
        assert!(matches!(text, MessageContent::Text(s) if s == "hello"));

        let blocks: MessageContent =
            serde_json::from_str(r#"[{"type":"text","text":"hi"}]"#).unwrap();
        assert!(matches!(blocks, MessageContent::Blocks(v) if v.len() == 1));
    }

    #[test]
    fn usage_default() {
        let usage = Usage::default();
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
        assert!(usage.cache_creation_input_tokens.is_none());
    }

    #[test]
    fn stop_reason_serialization() {
        assert_eq!(
            serde_json::to_string(&StopReason::EndTurn).unwrap(),
            "\"end_turn\""
        );
        assert_eq!(
            serde_json::to_string(&StopReason::ToolUse).unwrap(),
            "\"tool_use\""
        );
    }
}
