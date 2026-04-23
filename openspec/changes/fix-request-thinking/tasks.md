## 1. Core Enum

- [x] 1.1 Add `cc_core::ThinkingConfig` in `cc-core/src/message.rs`,
      serde-tagged (`#[serde(tag = "type", rename_all = "snake_case")]`)
      with variants `Adaptive`, `Enabled { budget_tokens: u32 }`,
      `Disabled` — matches the Anthropic SDK
      `BetaThinkingConfigParam` and TS `sideQuery.ts:169-177`.
- [x] 1.2 Provide `ThinkingConfig::enabled(u32)` constructor for the
      common explicit-budget case.
- [x] 1.3 Re-export at `cc_core` so `cc_api::request` can depend on
      the public API.

## 2. Request Field + Builder

- [x] 2.1 Add `thinking: Option<ThinkingConfig>` to
      `CreateMessageRequest` with
      `#[serde(skip_serializing_if = "Option::is_none")]`.
- [x] 2.2 Initialize to `None` in `CreateMessageRequest::new`.
- [x] 2.3 Add `with_thinking(ThinkingConfig) -> Self` builder
      following the existing `with_system` / `with_tools` /
      `with_max_tokens` naming.

## 3. Tests

- [x] 3.1 `cc-core/src/message.rs::tests` — four tests:
      - `thinking_enabled_serializes_with_budget` →
        `{"type":"enabled","budget_tokens":1024}`
      - `thinking_disabled_serializes_without_budget` →
        `{"type":"disabled"}`
      - `thinking_adaptive_serializes_without_budget` →
        `{"type":"adaptive"}`
      - `thinking_round_trip` — serialize then deserialize.
- [x] 3.2 `cc-api/src/request.rs::tests` — three tests:
      - `default_request_omits_thinking_on_wire` — no `thinking` key
        on a default `new()` request.
      - `with_thinking_emits_enabled_shape_on_wire` — full
        `CreateMessageRequest` JSON carries the enabled shape.
      - `with_thinking_emits_disabled_shape_on_wire` — carries
        disabled shape.

## 4. Sign-off

- [x] 4.1 `cargo check -p cc-api` + `cargo clippy -p cc-api --
      -D warnings` + `cargo test -p cc-api` clean (26/26 passing).
      `cargo test -p cc-core` also clean (41/41, including the four
      new thinking tests).
- [ ] 4.2 Wire a real caller (e.g. a classifier/summarizer path that
      wants `Disabled`, or the main engine turn that wants
      `Adaptive`) — deferred to the next batch; out of scope here
      since the field exists additively.
