## 1. New helper module

- [x] 1.1 Create `rust/crates/cc-query/src/cache_breakpoint.rs` with
      `pub fn tag_last_block_for_caching(messages: &mut Vec<MessageParam>)`.
      Must no-op on empty vec and on `MessageContent::Text` messages;
      must splat `CacheControl::ephemeral_unscoped()` onto the trailing
      block of the trailing message when `MessageContent::Blocks`.
- [x] 1.2 Register the module in `rust/crates/cc-query/src/lib.rs`
      (add `pub mod cache_breakpoint;` — follow the existing
      `pub mod engine;` style).
- [x] 1.3 Import the helper at the top of `engine.rs`:
      `use crate::cache_breakpoint::tag_last_block_for_caching;`.

## 2. Call-site wiring

- [x] 2.1 In `rust/crates/cc-query/src/engine.rs` inside
      `QueryEngine::run_turn`'s `loop { … }` body, insert
      `tag_last_block_for_caching(messages);` BEFORE the
      `CreateMessageRequest::new(..., messages.clone())` at line 189.
      The tag runs every iteration so the most recently pushed
      message (user / assistant-reconstruction / tool-result) carries
      the breakpoint.
- [x] 2.2 Do **not** remove any existing message pushes (user at line
      172; assistant reconstruction later in the loop; tool-result
      pushes). The tagger operates on whatever is currently last.

## 3. Unit tests

- [x] 3.1 `cache_breakpoint::tests::tag_empty_vec_is_noop` — calling
      the helper on `Vec::new()` must not panic and must leave the
      vector empty.
- [x] 3.2 `cache_breakpoint::tests::tag_trailing_block_of_trailing
      _message` — construct messages with the trailing message
      containing `[Text("a"), ToolUse{…}]`; call the helper; assert
      only the `ToolUse` block has `cache_control: Some(ephemeral)`.
      Assert the preceding `Text("a")` block remains `None`. Assert
      the earlier messages remain fully untagged.
- [x] 3.3 `cache_breakpoint::tests::tag_is_idempotent` — call the
      helper twice on the same vector; assert exactly one block
      carries the breakpoint (the second call is a no-op overwrite
      with the same value).
- [x] 3.4 `cache_breakpoint::tests::tag_skips_thinking_trailing_block`
      — trailing block is `ContentBlock::Thinking(…)`; call the
      helper; assert the Thinking block's `cache_control` is still
      `None` (the `with_cache_control` no-op path at
      `cc-core/src/message.rs:243-245` is doing the work, so the
      helper does not need its own type check).
- [x] 3.5 `cache_breakpoint::tests::tag_string_content_is_noop` —
      message content is `MessageContent::Text(..)`; helper returns
      without mutation.

## 4. Integration test

- [x] 4.1 Extend `rust/crates/cc-query/src/engine.rs::tests` with
      `request_body_carries_ephemeral_on_last_block`. Spin up a
      `QueryEngine` via the existing test harness pattern (the
      engine tests already use an `ApiClient` backed by a mocked
      transport — see the C3 regression tests at
      `engine.rs:1097+`). On turn 1 capture the serialised
      `CreateMessageRequest` JSON and assert:
      ```
      body["messages"].last()["content"].last()["cache_control"] == {"type": "ephemeral"}
      ```
      and assert that no earlier block in any message carries
      `cache_control`.

## 5. Documentation

- [x] 5.1 Update the `QueryEngine` system-prompt cache-tier contract
      comment at `engine.rs:47-60` to mention that the *message-level*
      trailing-block tagging is now automatic inside `run_turn` and
      that callers must not pre-tag messages themselves (double
      tagging is benign but misleading).

## 6. Verification

- [x] 6.1 `cargo fmt --all` clean.
- [x] 6.2 `cargo clippy -p cc-query --all-targets -- -D warnings`
      clean.
- [x] 6.3 `cargo test -p cc-query -p cc-core -p cc-api` — all green.
- [x] 6.4 Manual live-API smoke (optional, not gating CI): run
      `cargo run --bin claude -- -m "hello"` for 3 turns; enable
      `RUST_LOG=cc_api=debug` and confirm
      `usage.cache_read_input_tokens > 0` on turn 2+.

## 7. Sign-off

- [x] 7.1 Commit message references P0 #1 parity-gap closure
      (end-to-end this time).
- [x] 7.2 Update `.claude/plan/parity-gaps-2026-04-23.md` to cross-
      reference both `fix-content-block-cache-control` (types) and
      this change (wiring) against P0 #1.
- [x] 7.3 Update memory note `project_phase3_progress.md`
      §"Batches A/B/C landed" paragraph to flip P0 #1 from "dead
      plumbing" to "end-to-end live".
