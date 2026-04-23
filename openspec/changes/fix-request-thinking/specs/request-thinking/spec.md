## ADDED Requirements

### Requirement: Extended-thinking config on CreateMessageRequest

`CreateMessageRequest` in `cc-api` MUST expose an optional
`thinking` field typed as `cc_core::ThinkingConfig`. The enum's
serialized shape MUST match the Anthropic API's
`BetaThinkingConfigParam` (TS reference:
`src/utils/sideQuery.ts:169-193`):

- `Adaptive` → `{"type":"adaptive"}`
- `Enabled { budget_tokens }` → `{"type":"enabled","budget_tokens":N}`
- `Disabled` → `{"type":"disabled"}`

When the field is `None`, serialization MUST omit the key entirely
(via `#[serde(skip_serializing_if = "Option::is_none")]`) so
existing callers that do not opt in are byte-identical on the wire.

A `with_thinking` builder method MUST exist on
`CreateMessageRequest` in line with the existing `with_system` /
`with_tools` / `with_max_tokens` builders.

#### Scenario: Default request omits thinking on the wire
- **GIVEN** `CreateMessageRequest::new("claude-opus-4-7", vec![])`
- **WHEN** serialized with serde_json
- **THEN** the resulting JSON object has no `thinking` key

#### Scenario: Enabled-with-budget reaches the wire
- **GIVEN** `CreateMessageRequest::new("claude-opus-4-7", vec![])
  .with_thinking(ThinkingConfig::enabled(2048))`
- **WHEN** serialized
- **THEN** `json["thinking"] == {"type":"enabled","budget_tokens":2048}`

#### Scenario: Disabled reaches the wire
- **GIVEN** `CreateMessageRequest::new(…)
  .with_thinking(ThinkingConfig::Disabled)`
- **WHEN** serialized
- **THEN** `json["thinking"] == {"type":"disabled"}`

#### Scenario: Adaptive reaches the wire
- **GIVEN** `ThinkingConfig::Adaptive` on the request
- **WHEN** serialized
- **THEN** `json["thinking"] == {"type":"adaptive"}`

#### Scenario: Round-trip preserves variant
- **GIVEN** a `ThinkingConfig::Enabled { budget_tokens: 2048 }`
- **WHEN** serialized then deserialized
- **THEN** the deserialized value equals the original
