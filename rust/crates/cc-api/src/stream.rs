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
    Thinking { thinking: String },
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
    pub cache_creation_input_tokens: Option<u32>,
    pub cache_read_input_tokens: Option<u32>,
    // Per-block accumulation.
    blocks: Vec<BlockState>,
}

#[derive(Debug)]
enum BlockState {
    Text { text: String },
    Thinking { thinking: String, signature: Option<String> },
    ToolUse { id: String, name: String, json: String },
}

impl StreamAccumulator {
    pub fn apply(&mut self, event: &StreamEvent) {
        match event {
            StreamEvent::MessageStart { message } => {
                self.message_id = message.id.clone();
                self.model = message.model.clone();
                self.input_tokens = message.usage.input_tokens;
                self.cache_creation_input_tokens = message.usage.cache_creation_input_tokens;
                self.cache_read_input_tokens = message.usage.cache_read_input_tokens;
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
                    ContentBlockStartData::Thinking { thinking } => {
                        self.blocks[idx] = BlockState::Thinking {
                            thinking: thinking.clone(),
                            signature: None,
                        };
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
                        (BlockState::Thinking { thinking, .. }, ContentBlockDelta::ThinkingDelta { thinking: d }) => {
                            thinking.push_str(d);
                        }
                        (BlockState::Thinking { signature, .. }, ContentBlockDelta::SignatureDelta { signature: s }) => {
                            *signature = Some(s.clone());
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
                BlockState::Thinking { thinking, signature } if !thinking.is_empty() => {
                    Some(ContentBlock::Thinking(cc_core::ThinkingBlock {
                        thinking,
                        signature,
                    }))
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accumulator_text_deltas() {
        let mut acc = StreamAccumulator::default();

        acc.apply(&StreamEvent::MessageStart {
            message: MessageStartData {
                id: "msg_123".into(),
                kind: "message".into(),
                role: "assistant".into(),
                content: vec![],
                model: "claude-sonnet-4-6".into(),
                stop_reason: None,
                stop_sequence: None,
                usage: Usage { input_tokens: 100, output_tokens: 0, cache_creation_input_tokens: None, cache_read_input_tokens: None },
            },
        });

        acc.apply(&StreamEvent::ContentBlockStart {
            index: 0,
            content_block: ContentBlockStartData::Text { text: String::new() },
        });

        acc.apply(&StreamEvent::ContentBlockDelta {
            index: 0,
            delta: ContentBlockDelta::TextDelta { text: "Hello ".into() },
        });
        acc.apply(&StreamEvent::ContentBlockDelta {
            index: 0,
            delta: ContentBlockDelta::TextDelta { text: "world!".into() },
        });

        acc.apply(&StreamEvent::MessageDelta {
            delta: MessageDeltaData { stop_reason: Some(StopReason::EndTurn), stop_sequence: None },
            usage: MessageDeltaUsage { output_tokens: 10 },
        });

        assert_eq!(acc.message_id, "msg_123");
        assert_eq!(acc.model, "claude-sonnet-4-6");
        assert_eq!(acc.text(), "Hello world!");
        assert_eq!(acc.stop_reason, Some(StopReason::EndTurn));
        assert_eq!(acc.input_tokens, 100);
        assert_eq!(acc.output_tokens, 10);
    }

    #[test]
    fn accumulator_tool_use() {
        let mut acc = StreamAccumulator::default();

        acc.apply(&StreamEvent::MessageStart {
            message: MessageStartData {
                id: "msg_456".into(),
                kind: "message".into(),
                role: "assistant".into(),
                content: vec![],
                model: "claude-sonnet-4-6".into(),
                stop_reason: None,
                stop_sequence: None,
                usage: Usage::default(),
            },
        });

        acc.apply(&StreamEvent::ContentBlockStart {
            index: 0,
            content_block: ContentBlockStartData::ToolUse {
                id: "tu_001".into(),
                name: "Bash".into(),
                input: json!({}),
            },
        });

        acc.apply(&StreamEvent::ContentBlockDelta {
            index: 0,
            delta: ContentBlockDelta::InputJsonDelta { partial_json: r#"{"com"#.into() },
        });
        acc.apply(&StreamEvent::ContentBlockDelta {
            index: 0,
            delta: ContentBlockDelta::InputJsonDelta { partial_json: r#"mand":"ls"}"#.into() },
        });

        let content = acc.into_content();
        assert_eq!(content.len(), 1);
        match &content[0] {
            ContentBlock::ToolUse(tu) => {
                assert_eq!(tu.id, "tu_001");
                assert_eq!(tu.name, "Bash");
                assert_eq!(tu.input["command"], "ls");
            }
            _ => panic!("expected ToolUse"),
        }
    }

    #[test]
    fn stream_event_deserialization() {
        let json = r#"{"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-6","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":50,"output_tokens":0}}}"#;
        let event: StreamEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(event, StreamEvent::MessageStart { .. }));

        let json = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}"#;
        let event: StreamEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(event, StreamEvent::ContentBlockDelta { .. }));

        // Unknown event type should parse as Unknown
        let json = r#"{"type":"future_event_2026"}"#;
        let event: StreamEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(event, StreamEvent::Unknown));
    }
}
