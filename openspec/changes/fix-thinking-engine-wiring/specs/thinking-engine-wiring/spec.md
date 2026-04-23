# Spec — Engine-side thinking plumbing

## ADDED Requirements

### Requirement: Engine forwards a user-configured thinking setting

`QueryOptions` SHALL expose a `thinking: Option<ThinkingConfig>`
field. When `Some(cfg)`, `QueryEngine::run_turn` SHALL call
`CreateMessageRequest::with_thinking(cfg.clone())` during request
construction. When `None`, `with_thinking` SHALL NOT be called, so
the request body omits the `thinking` key entirely (preserves
pre-change behaviour byte-for-byte).

#### Scenario: Default options omit thinking on the wire

- **Given** `QueryOptions::default()` (thinking is `None`)
- **When** `run_turn` constructs the request body
- **Then** the serialised body has no `"thinking"` key

#### Scenario: Enabled budget reaches the wire

- **Given** `QueryOptions::thinking = Some(ThinkingConfig::Enabled {
  budget_tokens: 2048 })`
- **When** `run_turn` constructs the request body
- **Then** the serialised body contains
  `"thinking": {"type": "enabled", "budget_tokens": 2048}`

#### Scenario: Adaptive reaches the wire

- **Given** `QueryOptions::thinking = Some(ThinkingConfig::Adaptive)`
- **When** `run_turn` constructs the request body
- **Then** the serialised body contains
  `"thinking": {"type": "adaptive"}` with no `budget_tokens` key

### Requirement: CLI exposes `--thinking` with permissive parsing

The `claude` binary SHALL accept a `--thinking <BUDGET|adaptive|off>`
flag that resolves to a `ThinkingConfig` per these rules:

| Input | Output |
|---|---|
| `adaptive` | `ThinkingConfig::Adaptive` |
| `off`, `disabled`, `0` | `ThinkingConfig::Disabled` |
| integer ≥ 1024 and < `max_tokens` | `ThinkingConfig::Enabled { budget_tokens: N }` |
| integer ≥ `max_tokens` | `Enabled { budget_tokens: max_tokens - 1 }`; log a `tracing::warn!` |
| integer < 1024 | hard error "budget_tokens must be >= 1024" |
| anything else | hard error "expected integer budget, 'adaptive', or 'off'" |

#### Scenario: Integer over max_tokens is clamped with warning

- **Given** `--max-tokens 4096 --thinking 8000`
- **When** the CLI parser runs
- **Then** `QueryOptions::thinking = Some(Enabled { budget_tokens: 4095 })`
- **And** a `tracing::warn!` event was emitted mentioning both
  numbers

#### Scenario: Sub-minimum integer is rejected

- **Given** `--thinking 512`
- **When** the CLI parser runs
- **Then** `claude` exits non-zero with a message containing
  `"budget_tokens must be >= 1024"`

### Requirement: Settings file may set the default

`cc_config::UserSettings` AND `ProjectSettings` SHALL accept an
optional `thinking` field. Serialisation matches the TS wire shape
(`{"type": "enabled", "budget_tokens": N}` / `{"type": "disabled"}`
/ `{"type": "adaptive"}`). The CLI flag SHALL take precedence over
the settings value on conflict.

#### Scenario: Settings JSON round-trips

- **Given** `settings.json` contains
  `{"thinking": {"type": "enabled", "budget_tokens": 4096}}`
- **When** `cc_config::load` parses it
- **Then** `UserSettings::thinking == Some(Enabled { budget_tokens: 4096 })`

## MODIFIED Requirements

### Requirement: request-thinking spec coverage includes the adaptive scenario at the request level

The cc-api test suite MUST include a `CreateMessageRequest`-level test for the `ThinkingConfig::Adaptive` variant, closing the "Adaptive on wire" scenario in `specs/request-thinking/spec.md` at the request level (not just the enum level). The test `with_thinking_emits_adaptive_shape_on_wire` SHALL live in `rust/crates/cc-api/src/request.rs::tests`.

#### Scenario: Request-level adaptive test exists

- **Given** the cc-api test module
- **When** `cargo test -p cc-api with_thinking_emits_adaptive` runs
- **Then** the test passes and asserts the request body contains
  `"thinking":{"type":"adaptive"}` with no `budget_tokens`
