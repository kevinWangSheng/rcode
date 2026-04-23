## 1. QueryOptions extension

- [x] 1.1 Add `pub thinking: Option<cc_core::ThinkingConfig>` to
      `QueryOptions` in `rust/crates/cc-query/src/engine.rs:27-34`.
- [x] 1.2 Initialise to `None` in the `Default` impl at line 36-44.
- [x] 1.3 Re-export `ThinkingConfig` from `cc-query` if needed so
      downstream crates don't have to depend on cc-core just to
      construct the option.

## 2. run_turn wiring

- [x] 2.1 In `engine.rs` `run_turn` around line 189-197, after the
      existing `with_max_tokens` / `with_system` / `with_tools`
      chain, add:
      ```
      if let Some(cfg) = &self.options.thinking {
          req = req.with_thinking(cfg.clone());
      }
      ```
- [x] 2.2 Confirm `ThinkingConfig` implements `Clone` (it already
      does at `cc-core/src/message.rs:267`). No further derive
      changes needed.

## 3. CLI flag

- [x] 3.1 Add `#[arg(long, value_name = "BUDGET|adaptive|off")]
      pub thinking: Option<String>` to `Cli` in
      `rust/cc/src/main.rs` near `--max-tokens` (~line 64).
- [x] 3.2 Write a free function
      `fn parse_thinking(raw: &str, max_tokens: u32)
        -> Result<cc_core::ThinkingConfig, String>`:
      - `"adaptive"` → `ThinkingConfig::Adaptive`
      - `"off"` | `"disabled"` | `"0"` → `ThinkingConfig::Disabled`
      - parsable as `u32` and `>= 1024` → `Enabled { budget_tokens }`
      - parsable as `u32` but `< 1024` → error with hint
        "budget_tokens must be >= 1024 (Anthropic API minimum)"
      - parsable as `u32` and `>= max_tokens` → clamp to
        `max_tokens - 1`, log a `tracing::warn!` with both numbers
      - unparseable → error "expected integer budget, 'adaptive',
        or 'off'"
- [x] 3.3 After parsing, plumb into `QueryOptions::thinking` at the
      same site where `max_tokens` is forwarded.
- [x] 3.4 Unit-test the parser with five inputs: `"adaptive"`,
      `"off"`, `"2048"`, `"512"` (sub-minimum), `"abc"` (invalid).

## 4. Settings support (can land in a separate commit)

- [x] 4.1 Add `#[serde(default, skip_serializing_if = "Option::
      is_none")] pub thinking: Option<cc_core::ThinkingConfig>` to
      `UserSettings` / `ProjectSettings` in
      `rust/crates/cc-config/src/settings.rs`.
- [x] 4.2 Round-trip test: load a JSON snippet
      `{"thinking": {"type": "enabled", "budget_tokens": 4096}}` and
      assert the parsed struct matches.
- [x] 4.3 Precedence rule: CLI flag overrides settings. If the
      config layer already merges layered sources, just make sure
      the CLI value is applied last (document explicitly in
      `.claude/plan/implementation-notes.md` under the
      "Configuration precedence" heading).

## 5. Spec-level test (closes QA spec gap)

- [x] 5.1 Add `with_thinking_emits_adaptive_shape_on_wire` to
      `rust/crates/cc-api/src/request.rs::tests`, right after
      `with_thinking_emits_disabled_shape_on_wire`. Assert the
      serialised `CreateMessageRequest` has
      `"thinking":{"type":"adaptive"}` and no `budget_tokens` key.

## 6. Engine-level regression tests

- [x] 6.1 `engine::tests::thinking_omitted_when_options_unset` —
      `QueryOptions::default()` → request body has no `thinking`
      key.
- [x] 6.2 `engine::tests::thinking_enabled_budget_flows_to_request`
      — options set to `Enabled { budget_tokens: 2048 }` → request
      body has `{"thinking":{"type":"enabled","budget_tokens":
      2048}}`.
- [x] 6.3 `engine::tests::thinking_adaptive_flows_to_request` —
      assert body has `{"thinking":{"type":"adaptive"}}`.
- [x] 6.4 Reuse the captured-request pattern from the C3 regression
      tests at `engine.rs:1097+` so the mock-transport wiring is
      consistent.

## 7. Verification

- [x] 7.1 `cargo fmt --all` clean.
- [x] 7.2 `cargo clippy --workspace --all-targets -- -D warnings`
      clean.
- [x] 7.3 `cargo test -p cc-core -p cc-api -p cc-query -p cc-config
      -p claude-cli` — all green.
- [x] 7.4 Manual smoke: `cargo run --bin claude -- --thinking 2048
      -m "think step by step about 2+2"`. Enable `RUST_LOG=cc_api=
      debug` and confirm the request body contains the thinking
      field; response stream should include `StreamThinking` events
      (already accumulated by `StreamAccumulator`).

## 8. Sign-off

- [x] 8.1 Commit message references P0 #2 end-to-end closure.
- [x] 8.2 Update `.claude/plan/parity-gaps-2026-04-23.md` P0 #2 row
      to cross-reference both `fix-request-thinking` (types) and
      this change (wiring).
- [x] 8.3 Update memory `project_phase3_progress.md` Batch A
      paragraph to flip P0 #2 from "dead plumbing" to "end-to-end
      live".
