use cc_core::{ContentBlock, StopReason, Usage};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// All event types emitted by the Anthropic streaming API.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    MessageStart {
        message: MessageStartData,
    },
    ContentBlockStart {
        index: u32,
        content_block: ContentBlockStartData,
    },
    ContentBlockDelta {
        index: u32,
        delta: ContentBlockDelta,
    },
    ContentBlockStop {
        index: u32,
    },
    MessageDelta {
        delta: MessageDeltaData,
        usage: MessageDeltaUsage,
    },
    MessageStop,
    Ping,
    /// Catch-all for unknown event types (forward-compatibility).
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageStartData {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub role: String,
    pub content: Vec<Value>,
    pub model: String,
    pub stop_reason: Option<StopReason>,
    pub stop_sequence: Option<String>,
    pub usage: Usage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlockStartData {
    Text { text: String },
    ToolUse { id: String, name: String, input: Value },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlockDelta {
    TextDelta { text: String },
    InputJsonDelta { partial_json: String },
    ThinkingDelta { thinking: String },
    SignatureDelta { signature: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageDeltaData {
    pub stop_reason: Option<StopReason>,
    pub stop_sequence: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageDeltaUsage {
    pub output_tokens: u32,
}

/// Accumulates a streaming response into a completed message.
#[derive(Debug, Default)]
pub struct StreamAccumulator {
    pub message_id: String,
    pub model: String,
    pub stop_reason: Option<StopReason>,
    pub input_tokens: u32,
    pub output_tokens: u32,
    // Per-block accumulation.
    blocks: Vec<BlockState>,
}

#[derive(Debug)]
enum BlockState {
    Text { text: String },
    ToolUse { id: String, name: String, json: String },
}

impl StreamAccumulator {
    pub fn apply(&mut self, event: &StreamEvent) {
        match event {
            StreamEvent::MessageStart { message } => {
                self.message_id = message.id.clone();
                self.model = message.model.clone();
                self.input_tokens = message.usage.input_tokens;
            }
            StreamEvent::ContentBlockStart { index, content_block } => {
                let idx = *index as usize;
                // Grow if needed.
                while self.blocks.len() <= idx {
                    self.blocks.push(BlockState::Text { text: String::new() });
                }
                match content_block {
                    ContentBlockStartData::Text { text } => {
                        self.blocks[idx] = BlockState::Text { text: text.clone() };
                    }
                    ContentBlockStartData::ToolUse { id, name, .. } => {
                        self.blocks[idx] = BlockState::ToolUse {
                            id: id.clone(),
                            name: name.clone(),
                            json: String::new(),
                        };
                    }
                }
            }
            StreamEvent::ContentBlockDelta { index, delta } => {
                let idx = *index as usize;
                if let Some(block) = self.blocks.get_mut(idx) {
                    match (block, delta) {
                        (BlockState::Text { text }, ContentBlockDelta::TextDelta { text: d }) => {
                            text.push_str(d);
                        }
                        (BlockState::ToolUse { json, .. }, ContentBlockDelta::InputJsonDelta { partial_json }) => {
                            json.push_str(partial_json);
                        }
                        _ => {}
                    }
                }
            }
            StreamEvent::MessageDelta { delta, usage } => {
                self.stop_reason = delta.stop_reason;
                self.output_tokens = usage.output_tokens;
            }
            _ => {}
        }
    }

    /// Convert accumulated state into final content blocks.
    pub fn into_content(self) -> Vec<ContentBlock> {
        self.blocks
            .into_iter()
            .filter_map(|b| match b {
                BlockState::Text { text } if !text.is_empty() => {
                    Some(ContentBlock::text(text))
                }
                BlockState::ToolUse { id, name, json } => {
                    let input: Value = serde_json::from_str(&json).unwrap_or(Value::Object(Default::default()));
                    Some(ContentBlock::ToolUse(cc_core::ToolUseBlock {
                        id,
                        name,
                        input,
                    }))
                }
                _ => None,
            })
            .collect()
    }

    /// Return the accumulated text (joining all text blocks).
    pub fn text(&self) -> String {
        self.blocks
            .iter()
            .filter_map(|b| match b {
                BlockState::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }
}
