## 1. Core Types

- [x] 1.1 Add `cache_control: Option<CacheControl>` (with
      `#[serde(default, skip_serializing_if = "Option::is_none")]`) to
      `ToolUseBlock`, `ToolResultBlock`, and `ImageBlock` in
      `cc-core/src/message.rs`.
- [x] 1.2 Intentionally skip `ThinkingBlock` / `RedactedThinkingBlock`
      — TS `services/api/claude.ts:660-665` excludes them from cache
      tagging and the API treats them as non-cacheable.
- [x] 1.3 Add `ContentBlock::with_cache_control(CacheControl) -> Self`
      helper that tags the underlying variant and silently no-ops on
      Thinking / RedactedThinking / Unknown.

## 2. Struct-literal Migration

- [x] 2.1 `cc-api/src/stream.rs` — three `ToolUseBlock` literals in the
      stream accumulator's `into_content` + `into_content_recovering`
      paths updated to `cache_control: None`.
- [x] 2.2 `cc-query/src/engine.rs` — tool-loop literals (eight
      `ToolResultBlock`, five `ToolUseBlock`, one `ImageBlock` across
      runtime code + tests) updated.

## 3. Wire-shape Tests

- [x] 3.1 `cc-core/src/message.rs::tests` — seven new tests: absent-
      when-unset, per-variant emit-on-wire (Text / ToolUse /
      ToolResult / Image), thinking-block no-op, inbound round-trip.
- [x] 3.2 `cc-api/src/request.rs::tests` — two end-to-end tests
      (`last_content_block_cache_control_reaches_wire`,
      `tool_result_cache_control_reaches_wire`) that serialize a full
      `CreateMessageRequest` and assert
      `messages[0].content[last].cache_control ==
      {"type":"ephemeral"}`.

## 4. Sign-off

- [x] 4.1 `cargo check -p cc-api` + `cargo clippy -p cc-api --
      -D warnings` + `cargo test -p cc-api` clean (26/26 passing).
      `cargo test -p cc-core` also clean (41/41).
- [ ] 4.2 Live API smoke: observe `usage.cache_read_input_tokens > 0`
      on the second tool-loop turn after a caller tags the trailing
      block. Deferred to the next human-driven session (the same
      deferral as `fix-cache-control-scope §5.2`).
