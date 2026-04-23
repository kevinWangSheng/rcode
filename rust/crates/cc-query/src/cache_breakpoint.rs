//! Turn-level prompt-cache breakpoint placement.
//!
//! Mirrors TS `services/api/claude.ts::addCacheBreakpoints`: before every
//! `CreateMessageRequest`, the trailing content block of the trailing
//! message is tagged with `cache_control = ephemeral`. Everything up to
//! that block participates in the cache read; the block itself defines
//! the new cut. On multi-turn tool loops this drives `usage.cache_read
//! _input_tokens > 0` on turn 2+, which is the whole point of the
//! feature.
//!
//! System-block cache tiering is handled separately by the caller of
//! `QueryEngine::new` (see the `QueryEngine` doc comment). This module
//! only touches message-level blocks.

use cc_core::{CacheControl, MessageContent, MessageParam};

/// Tag the trailing content block of the trailing message with an
/// ephemeral cache breakpoint.
///
/// No-ops when:
/// - `messages` is empty.
/// - The trailing message's content is `MessageContent::Text` (string
///   content can't carry a breakpoint; TS has the same limitation).
/// - The trailing message's `Blocks` vector is empty.
/// - The trailing block is `Thinking` / `RedactedThinking` / `Unknown`
///   — `ContentBlock::with_cache_control` already no-ops on those
///   variants, matching TS exclusions.
///
/// Idempotent: calling twice leaves the same block tagged exactly once
/// with the same value.
pub fn tag_last_block_for_caching(messages: &mut [MessageParam]) {
    let Some(last_msg) = messages.last_mut() else {
        return;
    };
    let MessageContent::Blocks(blocks) = &mut last_msg.content else {
        return;
    };
    let Some(last_block) = blocks.pop() else {
        return;
    };
    blocks.push(last_block.with_cache_control(CacheControl::ephemeral_unscoped()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use cc_core::{ContentBlock, Role, ToolUseBlock};

    fn user_text(s: &str) -> MessageParam {
        MessageParam {
            role: Role::User,
            content: MessageContent::Text(s.into()),
        }
    }

    fn user_blocks(blocks: Vec<ContentBlock>) -> MessageParam {
        MessageParam {
            role: Role::User,
            content: MessageContent::Blocks(blocks),
        }
    }

    fn assistant_blocks(blocks: Vec<ContentBlock>) -> MessageParam {
        MessageParam {
            role: Role::Assistant,
            content: MessageContent::Blocks(blocks),
        }
    }

    fn cache_of(block: &ContentBlock) -> Option<&CacheControl> {
        match block {
            ContentBlock::Text(b) => b.cache_control.as_ref(),
            ContentBlock::ToolUse(b) => b.cache_control.as_ref(),
            ContentBlock::ToolResult(b) => b.cache_control.as_ref(),
            ContentBlock::Image(b) => b.cache_control.as_ref(),
            ContentBlock::Thinking(_)
            | ContentBlock::RedactedThinking(_)
            | ContentBlock::Unknown => None,
        }
    }

    #[test]
    fn tag_empty_vec_is_noop() {
        let mut msgs: Vec<MessageParam> = Vec::new();
        tag_last_block_for_caching(&mut msgs);
        assert!(msgs.is_empty());
    }

    #[test]
    fn tag_string_content_is_noop() {
        let mut msgs = vec![user_text("hi")];
        tag_last_block_for_caching(&mut msgs);
        // String content can't carry a breakpoint; stays unchanged.
        assert!(matches!(&msgs[0].content, MessageContent::Text(t) if t == "hi"));
    }

    #[test]
    fn tag_trailing_block_of_trailing_message() {
        let mut msgs = vec![
            user_blocks(vec![ContentBlock::text("earlier message")]),
            assistant_blocks(vec![
                ContentBlock::text("a"),
                ContentBlock::ToolUse(ToolUseBlock {
                    id: "tu_1".into(),
                    name: "Bash".into(),
                    input: serde_json::json!({}),
                    cache_control: None,
                }),
            ]),
        ];
        tag_last_block_for_caching(&mut msgs);

        // Earlier message untouched.
        let MessageContent::Blocks(earlier) = &msgs[0].content else {
            panic!("expected blocks");
        };
        assert!(cache_of(&earlier[0]).is_none());

        // Trailing message: first block untouched, trailing block tagged.
        let MessageContent::Blocks(last) = &msgs[1].content else {
            panic!("expected blocks");
        };
        assert!(
            cache_of(&last[0]).is_none(),
            "preceding text block untouched"
        );
        assert!(
            cache_of(&last[1]).is_some(),
            "trailing tool_use block tagged"
        );
    }

    #[test]
    fn tag_is_idempotent() {
        let mut msgs = vec![assistant_blocks(vec![ContentBlock::text("hi")])];
        tag_last_block_for_caching(&mut msgs);
        tag_last_block_for_caching(&mut msgs);
        let MessageContent::Blocks(blocks) = &msgs[0].content else {
            panic!("expected blocks");
        };
        assert_eq!(blocks.len(), 1, "no duplication");
        assert!(cache_of(&blocks[0]).is_some());
    }

    #[test]
    fn tag_skips_thinking_trailing_block() {
        use cc_core::ThinkingBlock;
        let mut msgs = vec![assistant_blocks(vec![
            ContentBlock::text("prose"),
            ContentBlock::Thinking(ThinkingBlock {
                thinking: "…".into(),
                signature: None,
            }),
        ])];
        tag_last_block_for_caching(&mut msgs);
        let MessageContent::Blocks(blocks) = &msgs[0].content else {
            panic!("expected blocks");
        };
        // Thinking block: with_cache_control is a no-op per cc-core, so no
        // tag lands anywhere on this message.
        assert!(cache_of(&blocks[0]).is_none());
        assert!(cache_of(&blocks[1]).is_none());
    }

    #[test]
    fn tag_empty_trailing_blocks_vec_is_noop() {
        let mut msgs = vec![assistant_blocks(Vec::new())];
        tag_last_block_for_caching(&mut msgs);
        let MessageContent::Blocks(blocks) = &msgs[0].content else {
            panic!("expected blocks");
        };
        assert!(blocks.is_empty());
    }
}
