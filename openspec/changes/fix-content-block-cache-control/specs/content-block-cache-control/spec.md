## ADDED Requirements

### Requirement: Content-block cache_control on the wire

Every user / assistant content block that the TS reference allows to
carry a prompt-cache breakpoint (`TextBlock`, `ToolUseBlock`,
`ToolResultBlock`, `ImageBlock`) MUST expose an optional
`cache_control` field. When set, serialization MUST emit the
breakpoint in the JSON body of `POST /v1/messages`; when unset, the
field MUST be omitted so existing wire fixtures do not regress.

`ThinkingBlock` and `RedactedThinkingBlock` MUST NOT expose a
`cache_control` field — TS `services/api/claude.ts:660-665` excludes
those variants from cache tagging and the API treats them as
non-cacheable.

Inbound deserialization MUST tolerate the field on any variant that
carries it so server round-trips are lossless.

A `ContentBlock::with_cache_control` helper MUST exist to attach a
breakpoint at construction time regardless of the concrete variant;
calling it on a Thinking / RedactedThinking / Unknown variant MUST be
a silent no-op (parity with the TS splat-on-last-block pattern).

#### Scenario: Text block emits cache_control on the wire
- **GIVEN** `ContentBlock::text("hi").with_cache_control(CacheControl::ephemeral_unscoped())`
- **WHEN** serialized with serde_json
- **THEN** the JSON is
  `{"type":"text","text":"hi","cache_control":{"type":"ephemeral"}}`

#### Scenario: Text block omits the field when unset
- **GIVEN** `ContentBlock::text("hi")`
- **WHEN** serialized
- **THEN** the JSON is `{"type":"text","text":"hi"}` with no
  `cache_control` key

#### Scenario: Tool_result block emits cache_control on the wire
- **GIVEN** a `ToolResultBlock { cache_control: Some(ephemeral), … }`
- **WHEN** serialized
- **THEN** the JSON carries
  `"cache_control":{"type":"ephemeral"}`

#### Scenario: Tool_use block emits cache_control on the wire
- **GIVEN** a `ToolUseBlock { cache_control: Some(ephemeral), … }`
- **WHEN** serialized
- **THEN** the JSON carries
  `"cache_control":{"type":"ephemeral"}`

#### Scenario: Image block emits cache_control on the wire
- **GIVEN** an `ImageBlock { cache_control: Some(ephemeral), … }`
- **WHEN** serialized
- **THEN** the JSON carries
  `"cache_control":{"type":"ephemeral"}`

#### Scenario: Thinking block refuses cache_control
- **GIVEN** `ContentBlock::Thinking(…).with_cache_control(…)`
- **WHEN** serialized
- **THEN** the JSON MUST NOT carry a `cache_control` key on the
  thinking block

#### Scenario: Trailing-block tagging reaches the full request body
- **GIVEN** a `CreateMessageRequest` whose last message contains two
  content blocks, the second tagged via
  `with_cache_control(ephemeral)`
- **WHEN** the request is serialized
- **THEN** `messages[0].content[0].cache_control` is absent and
  `messages[0].content[1].cache_control == {"type":"ephemeral"}`

#### Scenario: Inbound round-trip preserves cache_control
- **GIVEN** a server payload `{"type":"tool_use", …,
  "cache_control":{"type":"ephemeral"}}`
- **WHEN** deserialized into `ContentBlock`
- **THEN** deserialization succeeds and the block's `cache_control`
  field is `Some(CacheControl { kind: "ephemeral", … })`
