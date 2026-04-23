## Why

The 2026-04-23 parity audit (P0 #2) flagged that
`CreateMessageRequest` in `cc-api/src/request.rs` has no `thinking`
field. The TS client exposes it in
`src/utils/sideQuery.ts:58` (caller API: `thinking?: number | false`)
and serializes it at `sideQuery.ts:169-193` as:

- `{"type":"enabled","budget_tokens":N}` — explicit budget
- `{"type":"disabled"}` — explicit opt-out (needed on default-on models)
- `{"type":"adaptive"}` — server picks a budget per turn

Without this field, the Rust client cannot request extended thinking
and cannot disable it either. On models where thinking is default-on
(Opus/Sonnet 4.x per `src/utils/thinking.ts:146-162`), the server
may return thinking deltas the TUI is ready to render, but the Rust
caller has no programmatic way to *turn it off* for cheap classifier
paths (classifier prompts in TS explicitly set `thinking: false` to
skip the cost — see `sideQuery.ts:58,170-172`).

## What Changes

- Introduce `cc_core::ThinkingConfig` as a serde-tagged enum so the
  wire shape matches the Anthropic API's
  `BetaThinkingConfigParam`:
  ```rust
  pub enum ThinkingConfig {
      Adaptive,
      Enabled { budget_tokens: u32 },
      Disabled,
  }
  ```
- Expose `ThinkingConfig::enabled(u32)` for the common explicit-budget
  case (mirrors TS `sideQuery.ts:173-176`).
- Add `thinking: Option<ThinkingConfig>` to `CreateMessageRequest`
  with `#[serde(skip_serializing_if = "Option::is_none")]` so
  existing callers stay byte-identical on the wire.
- Add a `with_thinking` builder method on `CreateMessageRequest` for
  parity with the existing `with_system` / `with_tools` /
  `with_max_tokens` builders.
- Add unit tests in `cc-core::message` covering each wire shape plus
  a round-trip, and in `cc-api::request` asserting:
  - default request omits `thinking` entirely;
  - `with_thinking(enabled(N))` emits the `{"type":"enabled","budget_tokens":N}`
    shape at `request.thinking`;
  - `with_thinking(Disabled)` emits `{"type":"disabled"}`.

## Capabilities

### Added Capabilities
- `request-thinking`: `CreateMessageRequest` MUST accept an optional
  extended-thinking config, serialized in the wire shape the
  Anthropic API accepts today (parity with TS `sideQuery.ts`).

## Impact

- **Affected code:** `cc-core/src/message.rs` (new enum + tests),
  `cc-api/src/request.rs` (new field + builder + wire-shape tests).
  Callers in `cc-query::engine` / `cc-query::summarizer` are **not**
  touched in this change — they currently build requests without
  thinking and will continue to do so; wiring a caller that opts in
  is a downstream change once the request shape exists.
- **Wire format:** an *additive* field. When `None`, serialization
  is byte-identical to before. When `Some(...)`, emits the exact
  shape the Anthropic API already accepts on the TS client.
- **Risk:** LOW. Pure additive. No callers currently set the field.
  The enum is serde-tagged so adding a future variant (new `type`
  value) is a non-breaking addition on the wire.
