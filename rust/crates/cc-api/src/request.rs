use cc_core::{MessageParam, SystemBlock, ToolDefinition};
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
}
