## 1. Error Variant

- [x] 1.1 Add `StreamError::ToolInputNotJson { id, name, raw }` (truncate
      `raw` to first 2 KB for logging safety).
- [x] 1.2 In `StreamState::into_content`, return `Result<Vec<ContentBlock>,
      StreamError>` and propagate parse failures instead of `unwrap_or`.

## 2. Engine Translation

- [x] 2.1 In `cc-query/src/engine.rs`, catch `ToolInputNotJson` and emit a
      synthetic `tool_result` with `is_error: true`.
- [x] 2.2 Include the tool `id` so Claude pairs it correctly with the
      failed `tool_use`.

## 3. Tests

- [x] 3.1 Unit test in `cc-api` that drives a malformed JSON tail and
      asserts `StreamError::ToolInputNotJson`.
- [x] 3.2 Integration test in `cc-query` that drives the same through
      the tool loop and asserts an `is_error: true` tool_result is sent.

## 4. Sign-off

- [x] 4.1 `cargo test --workspace` + clippy clean.
