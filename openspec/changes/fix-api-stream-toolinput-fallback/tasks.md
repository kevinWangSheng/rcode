## 1. Error Variant

- [ ] 1.1 Add `StreamError::ToolInputNotJson { id, name, raw }` (truncate
      `raw` to first 2 KB for logging safety).
- [ ] 1.2 In `StreamState::into_content`, return `Result<Vec<ContentBlock>,
      StreamError>` and propagate parse failures instead of `unwrap_or`.

## 2. Engine Translation

- [ ] 2.1 In `cc-query/src/engine.rs`, catch `ToolInputNotJson` and emit a
      synthetic `tool_result` with `is_error: true`.
- [ ] 2.2 Include the tool `id` so Claude pairs it correctly with the
      failed `tool_use`.

## 3. Tests

- [ ] 3.1 Unit test in `cc-api` that drives a malformed JSON tail and
      asserts `StreamError::ToolInputNotJson`.
- [ ] 3.2 Integration test in `cc-query` that drives the same through
      the tool loop and asserts an `is_error: true` tool_result is sent.

## 4. Sign-off

- [ ] 4.1 `cargo test --workspace` + clippy clean.
