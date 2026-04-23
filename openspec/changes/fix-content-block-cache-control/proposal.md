## Why

The 2026-04-23 parity audit (P0 #1,
`.claude/plan/parity-gaps-2026-04-23.md`) flagged `cache_control` as
"never applied" on the Rust wire: prompt caching silently does nothing,
so every turn retransmits the full tool-use / tool-result tail and
burns ~a few thousand input tokens that the server would otherwise
read from the prefix cache.

The existing `fix-cache-control-scope` change covered **system
blocks** (attribution / static / dynamic tiering). It did NOT cover
user/assistant **content blocks**. In TS
(`src/services/api/claude.ts::addCacheBreakpoints`, lines 595-668) the
breakpoint is attached to the *trailing* content block of the most
recent message — which in an agentic turn is usually a
`tool_result`, `tool_use`, or `image` block. In the Rust types only
`TextBlock` and `SystemBlock` had a `cache_control` field;
`ToolUseBlock`, `ToolResultBlock`, and `ImageBlock` did not. Even if a
caller wanted to tag the right block, the field had nowhere to live
and the JSON body went out without it.

## What Changes

- Add `cache_control: Option<CacheControl>` to `ToolUseBlock`,
  `ToolResultBlock`, and `ImageBlock` in `cc-core::message`. Serde
  pattern matches the existing `TextBlock`/`SystemBlock`:
  `#[serde(default, skip_serializing_if = "Option::is_none")]` so
  existing callers and fixtures do not regress and round-tripping
  inbound payloads that happen to carry the field works defensively.
- `ThinkingBlock` / `RedactedThinkingBlock` intentionally do NOT get
  the field — TS
  (`src/services/api/claude.ts:660-665`) explicitly excludes those
  types from cache tagging, and the API treats them as non-cacheable.
- Add `ContentBlock::with_cache_control(CacheControl) -> Self` so
  callers can tag "the last block" via a one-liner chain regardless of
  the underlying variant. Thinking / RedactedThinking / Unknown are
  silent no-ops to preserve the TS contract.
- Update every in-tree struct-literal site (stream recovery paths,
  engine tool-result plumbing, test fixtures) to carry the new field
  explicitly (`cache_control: None`) — adding a required field is a
  breaking change to the struct literal, and `..Default::default()`
  is not available since the blocks carry mandatory content.
- Add wire-format tests in `cc-api::request` asserting
  `{"type":"ephemeral"}` reaches the serialized request body on both
  a trailing `TextBlock` and a trailing `ToolResultBlock`.
- Add per-variant unit tests in `cc-core::message` covering
  serialize-emit / serialize-skip / deserialize-inbound and the
  thinking-block no-op.

## Capabilities

### Added Capabilities
- `content-block-cache-control`: user/assistant content blocks MUST
  be able to carry a `cache_control` breakpoint on the wire, with
  the same ephemeral/scope shape the server accepts today on
  system blocks.

## Impact

- **Affected code:** `cc-core/src/message.rs` (new fields + helper +
  tests), `cc-api/src/request.rs` (wire-shape tests), `cc-api/src/stream.rs`
  (stream accumulator struct literals), `cc-query/src/engine.rs` (tool-loop
  struct literals).
- **Wire format:** outbound content blocks now *optionally* carry
  `cache_control`. When unset, serialization is byte-identical to
  before. When set, emits `{"type":"ephemeral"}` (scope currently
  always omitted per the 2026-04-17 wire-constraint finding in
  `fix-cache-control-scope/tasks.md §5.2`).
- **Cost:** lets the prompt-cache actually hit on tool-heavy turns —
  once a caller tags the trailing block, the static prefix reads
  from cache instead of being re-billed at full rate.
- **Risk:** LOW. Adding an optional `Option<_>` field with
  `skip_serializing_if` is additive and backwards compatible on
  both wire and in-memory shapes. Inbound deserialize is defensive
  (`default`). The struct-literal churn is mechanical and fully
  covered by the existing test suite in cc-api / cc-core / cc-query.
