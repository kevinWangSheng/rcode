# Proposal — Wire ThinkingConfig into the query engine

## Why

`fix-request-thinking` (commit `790a749`, 2026-04-23) added
`cc_core::ThinkingConfig` and `CreateMessageRequest::with_thinking`.
Wire-shape tests in `cc-core` and `cc-api` pass (enabled / disabled /
adaptive all serialise correctly; `skip_serializing_if = "Option::is
_none"` keeps the field off the wire when unset).

**But the builder has zero production callers.** The 2026-04-23 QA
pass:

```
$ rg -n "with_thinking|ThinkingConfig" rust/crates/cc-query rust/cc
# — no results (test-only references in cc-api and cc-core) —
```

And the proposal itself (task 4.2) explicitly deferred caller
integration:

> 4.2 Wire a real caller (e.g. a classifier/summarizer path that wants
> `Disabled`, or the main engine turn that wants `Adaptive`) —
> deferred to the next batch; out of scope here since the field
> exists additively.

End result: parity gap **P0 #2** (thinking settable on requests) is
still open end-to-end. A user running `claude -m "…"` has no way to
turn on extended thinking — no flag, no config key, no env var.

## Goal

Thread a user-expressible thinking preference through the stack:

```
CLI flag / settings.json / env var
      ↓
cc_config::UserSettings / ProjectSettings
      ↓
QueryOptions::thinking: Option<ThinkingConfig>
      ↓
QueryEngine::run_turn → CreateMessageRequest::with_thinking(...)
```

Also close the spec gap found by QA: the
`specs/request-thinking/spec.md` file requires an "Adaptive on wire"
scenario at the `CreateMessageRequest` level, but only the cc-core
enum is tested that way; the request-level test for adaptive is
missing.

## What changes

### 1. Add `thinking` to `QueryOptions`

```rust
// rust/crates/cc-query/src/engine.rs, line 27-34
pub struct QueryOptions {
    pub model: String,
    pub max_tokens: u32,
    pub non_interactive: bool,
    pub bypass_permissions: bool,
    /// Extended-thinking setting forwarded to every CreateMessageRequest
    /// as the `thinking` field. `None` means "omit the field entirely"
    /// (server default; matches current behaviour).
    pub thinking: Option<cc_core::ThinkingConfig>,
}

impl Default for QueryOptions {
    fn default() -> Self {
        Self {
            model: cc_core::models::DEFAULT.to_string(),
            max_tokens: 8192,
            non_interactive: false,
            bypass_permissions: false,
            thinking: None,
        }
    }
}
```

### 2. Use it in `run_turn`

```rust
// rust/crates/cc-query/src/engine.rs, around line 189-197:
let mut req = CreateMessageRequest::new(&self.options.model, messages.clone())
    .with_max_tokens(self.options.max_tokens);

if !self.system_blocks.is_empty() {
    req = req.with_system(self.system_blocks.clone());
}
if !tool_defs.is_empty() {
    req = req.with_tools(tool_defs.clone());
}
if let Some(cfg) = &self.options.thinking {
    req = req.with_thinking(cfg.clone());
}
```

(Cheap clone; `ThinkingConfig` is a small enum with `Copy`-ish inner
types. If the builder signature takes by value and the option needs
to stay in `QueryOptions`, a `.clone()` here is fine.)

### 3. Expose as a CLI flag

Add to `rust/cc/src/main.rs` `struct Cli` (near
`--max-tokens`, around line 64):

```rust
/// Extended-thinking budget, in tokens. Matches the TS `--thinking`
/// flag. Pass `0` or omit to disable; pass `adaptive` to let the
/// server pick a budget per turn.
///
/// Examples:
///   --thinking 2048      → { type: "enabled", budget_tokens: 2048 }
///   --thinking adaptive  → { type: "adaptive" }
///   --thinking off       → { type: "disabled" }
///   (omitted)            → field not sent
#[arg(long, value_name = "BUDGET|adaptive|off")]
thinking: Option<String>,
```

Then a small parser `parse_thinking(&str) -> Result<ThinkingConfig,
String>`:

- `"adaptive"` → `ThinkingConfig::Adaptive`
- `"off"`, `"disabled"` → `ThinkingConfig::Disabled`
- `"0"` → `ThinkingConfig::Disabled` (numeric alias for off)
- positive integer → `ThinkingConfig::Enabled { budget_tokens: N }`,
  enforcing `N >= 1024` (TS minimum; return a readable error below
  that)
- anything else → readable error

Threaded into `QueryOptions` at the same place `max_tokens` is passed
today.

### 4. (Optional, recommended) Expose in settings.json

Add a `thinking` field to `cc_config::settings::UserSettings`:

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub thinking: Option<cc_core::ThinkingConfig>,
```

Serde tagged-enum serialisation is already byte-identical to the TS
wire form, so a `settings.json` snippet

```json
{ "thinking": { "type": "enabled", "budget_tokens": 4096 } }
```

round-trips cleanly. Precedence: CLI flag overrides settings (same
pattern as `--model`).

If this lands in a separate follow-up, drop this section and flag
settings-level support in tasks.md §6.

### 5. Close the spec test gap

Add the missing `with_thinking_emits_adaptive_shape_on_wire` test to
`rust/crates/cc-api/src/request.rs::tests` — spec requires it but the
current test module only covers enabled / disabled / omitted. The
cc-core enum test covers adaptive in isolation, but the spec
scenario is at the request level.

### 6. Regression tests for the new wiring

- `engine::tests::thinking_omitted_when_options_unset` — build an
  engine with `thinking: None`, run one turn, assert the captured
  request body has no `thinking` key.
- `engine::tests::thinking_enabled_budget_flows_to_request` — build
  with `thinking: Some(Enabled { budget_tokens: 2048 })`, assert
  captured body has `{"thinking": {"type": "enabled", "budget_tokens":
  2048}}`.
- `engine::tests::thinking_adaptive_flows_to_request` — same shape,
  assert `{"thinking": {"type": "adaptive"}}`.
- `cli::tests::parse_thinking_flag_variants` — unit test the
  argument parser for all four forms (number, adaptive, off, invalid).

## Impact

- **Affected specs**: `request-thinking` (adds the spec-required
  "Adaptive on wire" scenario at the request level).
- **Affected crates**: `cc-query` (QueryOptions field + call site +
  tests), `cc` (CLI flag + parser + plumbing), optionally `cc-config`
  (settings field).
- **Wire compatibility**: additive; default `None` means the field is
  still omitted from the request body, preserving today's behaviour
  byte-for-byte.
- **User impact**: closes parity gap P0 #2; users can now pass
  `--thinking 4096` or put `"thinking"` in settings.json.

## Open questions

1. Should `max_tokens` be auto-clamped when thinking is enabled? TS
   clamps `budget_tokens` to `max_tokens - 1`. The Rust `ThinkingConfig`
   doc-comment at `cc-core/src/message.rs:259-261` says "callers must
   ensure `budget_tokens < max_tokens`". If we want full TS parity on
   error-avoidance, the parser (step 3) should emit a warning when
   `budget >= max_tokens` and clamp. Propose: warn + clamp inside
   `parse_thinking`, not inside the engine.
2. Env-var override? TS honours `ANTHROPIC_THINKING_BUDGET`. Low-cost
   add; propose as a ~5-LOC task in §4 follow-up.
