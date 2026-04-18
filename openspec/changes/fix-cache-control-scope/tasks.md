## 1. Core Types

- [ ] 1.1 Add `CacheControl::ephemeral_global()` / `ephemeral_org()` /
      `ephemeral_unscoped()` constructors in `cc-core/src/types.rs`.
- [ ] 1.2 Unit test that each constructor produces the expected `kind` +
      `scope` pair.

## 2. Main Binary

- [ ] 2.1 In `cc/src/main.rs::build_system_blocks`, replace the literal
      `CacheControl { kind: "ephemeral", scope: None }` with the correct
      helper: attribution → `None`, static → `ephemeral_global`,
      git/dynamic → `ephemeral_org`.
- [ ] 2.2 Keep the block ordering (attribution first, then static, then
      dynamic) so the server caches them in the right tier.

## 3. Query Engine

- [ ] 3.1 In `cc-query/src/engine.rs` wherever a memory block or other
      dynamic system block is emitted, use `ephemeral_org`.
- [ ] 3.2 If any path re-builds the static instruction prompt, use
      `ephemeral_global`.

## 4. Regression Test

- [ ] 4.1 Fixture test under `cc/tests/` (or the closest existing harness)
      that builds a system-prompt array and asserts:
      - block 0 (attribution): `cache_control = None`
      - block 1 (static instruction): `cache_control.scope == "global"`
      - block N (git / memory): `cache_control.scope == "org"`
- [ ] 4.2 Snapshot / JSON assertion to guard the exact wire shape
      (`"cache_control": {"type": "ephemeral", "scope": "global"}`).

## 5. Sign-off

- [ ] 5.1 `cargo test --workspace` + `cargo clippy --workspace -- -D warnings`
      clean.
- [ ] 5.2 One manual session: observe `usage.cache_read_input_tokens` > 0 on
      the second turn (proves global cache is hitting).
