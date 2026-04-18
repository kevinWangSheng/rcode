## 1. Core Types

- [x] 1.1 Add `CacheControl::ephemeral_global()` / `ephemeral_org()` /
      `ephemeral_unscoped()` constructors in `cc-core/src/message.rs`
      (CacheControl lives in message.rs, not types.rs).
- [x] 1.2 Unit test that each constructor produces the expected `kind` +
      `scope` pair (6 tests covering constructors + serde wire shape).

## 2. Main Binary

- [x] 2.1 In `cc/src/main.rs::build_system_blocks`, replace the literal
      `CacheControl { kind: "ephemeral", scope: None }` with the correct
      helper: attribution → `None`, static → `ephemeral_global`,
      git/dynamic → `ephemeral_org`.
- [x] 2.2 Block ordering preserved (attribution → static → git → memory →
      add_dirs). Split hot path into `build_system_blocks_inner` so the
      regression test can drive it with deterministic fixtures.

## 3. Query Engine

- [x] 3.1 In `cc-query/src/agent_runner.rs` the sub-agent override system
      block now uses `ephemeral_org` (dynamic, stable per sub-agent run).
- [x] 3.2 `cc-query/src/engine.rs` does not re-build static system blocks
      itself — it reuses whatever `main.rs` passes in, so no change needed.

## 4. Regression Test

- [x] 4.1 `cc/src/main.rs::tests::three_tier_tagging_attribution_static_dynamic`
      + `three_tier_tagging_handles_absent_dynamic_blocks` assert the
      tier layout.
- [x] 4.2 `three_tier_tagging_serialized_wire_shape` snapshots the exact
      on-wire JSON: attribution has no `cache_control`, static block is
      `{"type":"ephemeral","scope":"global"}`, memory is `"scope":"org"`.

## 5. Sign-off

- [x] 5.1 `cargo test --workspace` (430 passes, 0 failures) + `cargo clippy
      --workspace --all-targets -- -D warnings` clean.
- [ ] 5.2 One manual session: observe `usage.cache_read_input_tokens` > 0 on
      the second turn (proves global cache is hitting). — Requires live
      API; deferred to the next human-driven smoke test.
