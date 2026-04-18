## ADDED Requirements

### Requirement: Hard Error on Malformed Tool Input

The streaming client SHALL surface a `StreamError::ToolInputNotJson`
when the accumulated input buffer for a `tool_use` block fails
`serde_json::from_str` at end-of-stream. The client MUST NOT substitute
an empty `Value::Object` and proceed as if the tool call were
well-formed.

The query engine SHALL translate this error into a synthetic
`tool_result` block with `is_error: true`, paired by id to the failing
`tool_use`, so that Claude observes the failure and can retry.

#### Scenario: Truncated JSON at end of stream
- **GIVEN** an InputJsonDelta sequence that ends mid-object
  (e.g. `{"file_path":"/tmp/x","content`)
- **WHEN** the stream completes
- **THEN** `StreamState::into_content` returns
  `Err(StreamError::ToolInputNotJson { .. })`
- **AND** the query engine emits a `tool_result` with `is_error: true`
  whose content references the parse failure

#### Scenario: Well-formed tool input still succeeds
- **GIVEN** an InputJsonDelta sequence that assembles a valid JSON object
- **WHEN** the stream completes
- **THEN** a `ToolUse` content block is emitted with the parsed `input`
- **AND** no `StreamError` is produced
