## Why

`RUST_REWRITE_PLAN.md` §3 ("Must Match Exactly — Anthropic API Wire Format")
requires the system prompt to be split into three cache tiers:

> attribution (uncached) → static (global cache) → dynamic (org cache)

The current Rust implementation sets `scope: None` on every system block it
emits. Concretely:

- `cc/src/main.rs:549-551` — static attribution/instruction block
- `cc-query/src/engine.rs` — memory block path (same pattern)

With `scope: None` the server treats the entry as an unscoped ephemeral cache
request. The three-tier scheme collapses into a single pool, so the
`static` and `dynamic` blocks do not benefit from the longer-lived `global` /
`org` caches. Every turn retransmits the full system prompt (~a few thousand
tokens). This is a contract violation, a wire-format regression against the
TypeScript client, and a silent cost multiplier in production.

## What Changes

- Introduce an ergonomic `cc_core::CacheControl::{global, org, ephemeral}`
  constructor set so call sites cannot accidentally leave `scope` empty.
- At `cc/src/main.rs::build_system_blocks`, tag blocks explicitly:
  - attribution text → no `cache_control` (matches TS uncached tier)
  - static instruction block → `cache_control = ephemeral(global)`
  - git context / memory / other dynamic blocks → `cache_control = ephemeral(org)`
- Mirror the same tagging inside `cc-query::engine` wherever system blocks are
  assembled (agent runner memory path).
- Add a unit test that asserts the three-tier tagging on a fixed fixture so
  future regressions fail loudly.

## Capabilities

### Modified Capabilities
- `system-prompt-caching`: system blocks MUST carry the correct
  `cache_control.scope` according to the three-tier contract.

## Impact

- **Affected code:** `cc-core/src/types.rs` (constructor helpers),
  `cc/src/main.rs` (build_system_blocks), `cc-query/src/engine.rs`
  (agent-runner memory path).
- **Wire format:** outbound `system` array shape changes
  (`cache_control.scope` populated). Fully backwards compatible — we are
  starting to send a field we already should have been sending.
- **Cost:** each turn in an ongoing session should drop by a few thousand
  input tokens once global/org caches warm up.
- **Risk:** LOW. The Anthropic API already accepts the field; our own code
  path is small and covered by the new test.
