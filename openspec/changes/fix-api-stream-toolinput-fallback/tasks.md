## 1. Error Variant

- [x] 1.1 Add `StreamError::ToolInputNotJson { id, name, raw }` (truncate
      `raw` to first 2 KB for logging safety).
- [x] 1.2 In `StreamState::into_content`, return `Result<Vec<ContentBlock>,
      StreamError>` and propagate parse failures instead of `unwrap_or`.

## 2. Engine Translation

- [x] 2.1 In `cc-query/src/engine.rs`, catch `ToolInputNotJson` and emit a
      synthetic `tool_result` with `is_error: true`.
      Re-closed 2026-04-18: prior `[x]` mark was incorrect — an audit
      found that `grep ToolInputNotJson rust/crates/cc-query/` returned
      zero matches at HEAD=5c6fd88. `drain_stream` mapped every
      `StreamError` to a flat `CcError::api(...)` that aborted the
      whole turn instead of letting the model retry. Now genuinely
      implemented: `StreamAccumulator` gained
      `into_message_and_usage_recovering()` which emits placeholder
      `ToolUseBlock { input: {} }` (id pairing survives) plus a
      `Vec<(id, name, raw)>` of bad inputs; `drain_stream` returns
      that list; `run_turn` splits tool_use_blocks into valid
      (execute) + bad (synthesize `is_error: true` via
      `synthesize_bad_tool_input_result`) and re-orders via
      `merge_tool_results` to preserve the API-required 1:1 pairing.
- [x] 2.2 Include the tool `id` so Claude pairs it correctly with the
      failed `tool_use`.
      Re-closed 2026-04-18: `synthesize_bad_tool_input_result` sets
      `tool_use_id = id` and `merge_tool_results` re-orders results to
      match the `tool_use_blocks` sequence, so every bad id in the
      assistant turn N has its matching `tool_result` in user turn
      N+1.

## 3. Tests

- [x] 3.1 Unit test in `cc-api` that drives a malformed JSON tail and
      asserts `StreamError::ToolInputNotJson`.
      Plus 2026-04-18: three `recovering_path_*` tests in
      `cc-api/src/stream.rs` cover the new
      `into_content_recovering` variant
      (placeholder emission, empty-bad-list on well-formed input,
      mixed valid+bad order preservation).
- [x] 3.2 Integration test in `cc-query` that drives the same through
      the tool loop and asserts an `is_error: true` tool_result is sent.
      Re-closed 2026-04-18: `drain_stream_recovers_malformed_tool_use_into_bad_list`
      drives a crafted `mpsc` stream into the real
      `drain_stream`; `full_h1_path_builds_is_error_user_message_with_matching_id`
      then feeds the result through `merge_tool_results` exactly as
      `run_turn` does, serializes the user message, and asserts the
      on-wire shape carries `type=tool_result, is_error=true,
      tool_use_id=<matching>`;
      `merge_tool_results_preserves_order_and_synthesises_errors`
      exercises the reorder + synthesize split directly.

## 4. Sign-off

- [x] 4.1 `cargo test --workspace` + clippy clean.
      Re-verified 2026-04-18 (workspace: 45 binaries / 463+ passed /
      0 failed; clippy `--all-targets -- -D warnings` clean; openspec
      `validate --all --strict` 20/20).
