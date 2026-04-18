use cc_core::{CcError, ContentBlock, StopReason, Usage};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// Maximum size of the raw JSON buffer preserved in a [`StreamError::ToolInputNotJson`]
/// error. Buffers longer than this are truncated with a `"...(truncated)"` suffix
/// so logs and `is_error` tool_result payloads stay bounded.
const TOOL_INPUT_RAW_MAX: usize = 2048;

/// Errors that arise while converting an accumulated SSE stream into its final
/// content blocks. These are structured on purpose so callers (notably
/// `cc-query`) can translate them into user-visible retry feedback rather than
/// passing silent garbage (e.g. empty tool arguments) back to the model.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StreamError {
    /// The accumulated JSON buffer for a `tool_use` block could not be parsed
    /// at end-of-stream. `raw` is truncated to `TOOL_INPUT_RAW_MAX` bytes.
    #[error("tool_use input for tool '{name}' (id {id}) was not valid JSON: {raw}")]
    ToolInputNotJson { id: String, name: String, raw: String },
}

impl From<StreamError> for CcError {
    fn from(e: StreamError) -> Self {
        CcError::Api(e.to_string())
    }
}

fn truncate_raw(raw: &str) -> String {
    if raw.len() <= TOOL_INPUT_RAW_MAX {
        raw.to_string()
    } else {
        // Truncate on a char boundary to avoid splitting a multi-byte scalar.
        let mut end = TOOL_INPUT_RAW_MAX;
        while end > 0 && !raw.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...(truncated)", &raw[..end])
    }
}

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
                self.stop_reason = delta.stop_reason.clone();
                self.output_tokens = usage.output_tokens;
            }
            _ => {}
        }
    }

    /// Convert accumulated state into final content blocks.
    ///
    /// Returns `StreamError::ToolInputNotJson` if any `tool_use` block's
    /// accumulated JSON buffer fails to parse at end-of-stream. We surface
    /// this as an explicit error rather than falling back to `{}`, because
    /// an empty object silently turns into `bash -c ""`, `Write` with no
    /// path, etc. The caller (`cc-query`) translates this into a
    /// `tool_result` with `is_error: true` so the model can self-correct.
    pub fn into_content(self) -> Result<Vec<ContentBlock>, StreamError> {
        let mut out = Vec::with_capacity(self.blocks.len());
        for b in self.blocks {
            match b {
                BlockState::Text { text } if !text.is_empty() => {
                    out.push(ContentBlock::text(text));
                }
                BlockState::Text { .. } => {}
                BlockState::ToolUse { id, name, json } => {
                    let input: Value = serde_json::from_str(&json).map_err(|_| {
                        StreamError::ToolInputNotJson {
                            id: id.clone(),
                            name: name.clone(),
                            raw: truncate_raw(&json),
                        }
                    })?;
                    out.push(ContentBlock::ToolUse(cc_core::ToolUseBlock {
                        id,
                        name,
                        input,
                    }));
                }
            }
        }
        Ok(out)
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

    fn drive_events(acc: &mut StreamAccumulator, events: &[StreamEvent]) {
        for ev in events {
            acc.apply(ev);
        }
    }

    fn tool_use_start(index: u32, id: &str, name: &str) -> StreamEvent {
        StreamEvent::ContentBlockStart {
            index,
            content_block: ContentBlockStartData::ToolUse {
                id: id.to_string(),
                name: name.to_string(),
                input: Value::Object(Default::default()),
            },
        }
    }

    fn input_json_delta(index: u32, partial: &str) -> StreamEvent {
        StreamEvent::ContentBlockDelta {
            index,
            delta: ContentBlockDelta::InputJsonDelta {
                partial_json: partial.to_string(),
            },
        }
    }

    /// Crafted SSE stream with a truncated tool_use JSON buffer: the
    /// accumulated `partial_json` deltas never close the outer object
    /// (ends mid-string). The fallback `unwrap_or({})` would have silently
    /// handed the tool an empty-args call; this test asserts we now surface
    /// a typed `ToolInputNotJson` error carrying the raw fragment.
    #[test]
    fn malformed_tool_use_json_returns_error_instead_of_empty_object() {
        let mut acc = StreamAccumulator::default();
        drive_events(
            &mut acc,
            &[
                tool_use_start(0, "tool_abc", "Write"),
                input_json_delta(0, r#"{"file_path":"/tmp/x","content"#),
                // Stream ends here — JSON is truncated.
            ],
        );

        let err = acc.into_content().expect_err("malformed JSON must error");
        match err {
            StreamError::ToolInputNotJson { id, name, raw } => {
                assert_eq!(id, "tool_abc");
                assert_eq!(name, "Write");
                assert!(raw.contains("/tmp/x"), "raw should include buffer fragment: {raw}");
                assert!(raw.contains("\"content"), "raw should show truncation point: {raw}");
            }
        }
    }

    #[test]
    fn well_formed_tool_use_json_parses_normally() {
        let mut acc = StreamAccumulator::default();
        drive_events(
            &mut acc,
            &[
                tool_use_start(0, "tool_1", "Bash"),
                input_json_delta(0, r#"{"command":"ls"}"#),
            ],
        );
        let blocks = acc.into_content().expect("well-formed JSON must succeed");
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            ContentBlock::ToolUse(tu) => {
                assert_eq!(tu.id, "tool_1");
                assert_eq!(tu.name, "Bash");
                assert_eq!(tu.input["command"], "ls");
            }
            _ => panic!("expected ToolUse block"),
        }
    }

    #[test]
    fn text_block_before_malformed_tool_use_still_errors() {
        // The first text block is well-formed, but the trailing tool_use
        // is malformed. `into_content` returns the error (per proposal,
        // the engine catches it and synthesizes a tool_result). The raw
        // fragment must be preserved so the engine can surface it.
        let mut acc = StreamAccumulator::default();
        drive_events(
            &mut acc,
            &[
                StreamEvent::ContentBlockStart {
                    index: 0,
                    content_block: ContentBlockStartData::Text { text: String::new() },
                },
                StreamEvent::ContentBlockDelta {
                    index: 0,
                    delta: ContentBlockDelta::TextDelta {
                        text: "Calling tool now...".to_string(),
                    },
                },
                tool_use_start(1, "tool_xyz", "Edit"),
                input_json_delta(1, r#"{"file_path":"a","old_stri"#),
            ],
        );
        let err = acc.into_content().expect_err("must error");
        let StreamError::ToolInputNotJson { id, name, raw } = err;
        assert_eq!(id, "tool_xyz");
        assert_eq!(name, "Edit");
        assert!(raw.contains("old_stri"));
    }

    #[test]
    fn truncate_raw_caps_at_two_kb_on_char_boundary() {
        let long = "a".repeat(5_000);
        let out = truncate_raw(&long);
        assert!(out.len() <= TOOL_INPUT_RAW_MAX + "...(truncated)".len());
        assert!(out.ends_with("...(truncated)"));

        // Short input is passed through untouched.
        let short = "short";
        assert_eq!(truncate_raw(short), "short");

        // Multi-byte boundary: build a string whose 2048th byte falls mid-glyph.
        // A 3-byte character (e.g. 'é' is 2 bytes, 'あ' is 3 bytes) lets us
        // exercise the char-boundary walk-back.
        let mut s = "a".repeat(TOOL_INPUT_RAW_MAX - 1);
        s.push('あ'); // straddles the 2048-byte mark
        let out = truncate_raw(&s);
        assert!(out.ends_with("...(truncated)"));
        // Must still be valid UTF-8 and no split scalar.
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }

    #[test]
    fn stream_error_converts_into_cc_error() {
        let e = StreamError::ToolInputNotJson {
            id: "t1".into(),
            name: "Bash".into(),
            raw: "{\"cmd".into(),
        };
        let converted: CcError = e.into();
        match converted {
            CcError::Api(msg) => assert!(msg.contains("not valid JSON")),
            other => panic!("unexpected CcError variant: {other:?}"),
        }
    }
}
