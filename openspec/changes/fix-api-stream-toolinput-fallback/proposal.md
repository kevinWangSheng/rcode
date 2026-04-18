## Why

`cc-api/src/stream.rs:174` converts an accumulated ToolUse JSON string
into a `Value` with:

```rust
let input: Value = serde_json::from_str(&json).unwrap_or(Value::Object(Default::default()));
```

This runs at end-of-stream, not mid-stream. If the accumulated buffer
fails to parse *at that point*, the arguments are actually broken. The
fallback quietly hands the model an empty `{}` object and the tool
(Bash, Edit, Write, ...) is then invoked with **no arguments** — running
`bash -c ""`, writing a file with no path, etc. The error is invisible
to the user and to the model until the tool produces a confusing
downstream failure.

## What Changes

- Replace the silent fallback with a hard error:
  - on parse failure, return a new `StreamError::ToolInputNotJson { id,
    name, raw }` carrying the accumulated (truncated if huge) buffer.
- In `cc-query`'s tool-loop, treat this error by emitting a
  `tool_result` with `is_error: true` and a message pointing at the
  parse failure, so Claude sees it and can retry.
- Do not panic the stream; continue delivering any other blocks in the
  final message.
- Unit test: feed a buffer ending in `{"file_path":"/tmp/x","content` and
  assert the emitted tool_result has `is_error: true` and contains the
  raw fragment.

## Capabilities

### Modified Capabilities
- `api-stream-tool-use`: end-of-stream tool input parse failures MUST
  surface as an error, never as an empty input object.

## Impact

- **Affected code:** `cc-api/src/stream.rs`, `cc-query/src/engine.rs`
  (error translation).
- **Risk:** LOW. The failure mode we're removing is already wrong; the
  new behavior is strictly more truthful.
