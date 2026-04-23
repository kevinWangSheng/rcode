## ADDED Requirements

### Requirement: Async-Rewake Exit-Code-2 Semantics

A hook configured with `async_rewake: true` that exits with code 2
SHALL be reported via a distinct `HookOutcome::AsyncRewake(message)`
variant, where `message` is the hook's stdout trimmed (falling back
to stderr if stdout is empty). The runner SHALL short-circuit
remaining hooks for that event, the same as the legacy `Block`
path.

A hook *without* `async_rewake: true` retains the legacy `Block`
behaviour on exit 2.

#### Scenario: Default hook blocks on exit 2
- **GIVEN** a hook with no `async_rewake` field and `command` that exits with code 2
- **WHEN** the runner fires the hook's event
- **THEN** `HookRunResult.outcome` is `HookOutcome::Block(…)`
- **AND** `HookRunResult::is_halting()` is `true`

#### Scenario: asyncRewake hook yields AsyncRewake on exit 2
- **GIVEN** a hook with `async_rewake: true` and `command` that prints "wake" then exits with code 2
- **WHEN** the runner fires the hook's event
- **THEN** `HookRunResult.outcome` is `HookOutcome::AsyncRewake(msg)` where `msg` contains "wake"
- **AND** `HookRunResult::is_halting()` is `true`

#### Scenario: Both snake_case and camelCase accepted
- **GIVEN** a settings file with `{"command": "x", "asyncRewake": true}`
- **WHEN** the file is deserialised into `HookConfig`
- **THEN** `HookConfig::is_async_rewake()` returns `true`
- **AND** an equivalent `{"async_rewake": true}` entry deserialises to the same result

### Requirement: Additional-Context Aggregation

`HookRunner::run` SHALL parse each hook's stdout as JSON when the
hook exits with code 0, and SHALL append any non-empty
`additionalContext` field it finds to the returned
`HookRunResult.additional_contexts` vector, preserving hook
execution order.

Parse failures SHALL be silent — hooks are free to emit free-form
text on stdout, and only the JSON form opts into additional-context
collection.

`Block` and `AsyncRewake` outcomes SHALL short-circuit the chain —
subsequent hooks' additional-context payloads are NOT collected.

Consumers (the query engine) SHALL drain
`HookRunResult.additional_contexts` into the outgoing message list
as synthetic user messages before the next API request.

#### Scenario: JSON additionalContext is lifted onto HookRunResult
- **GIVEN** a hook whose stdout is `{"additionalContext":"Project is in maintenance mode"}` (exit 0)
- **WHEN** the runner fires the event
- **THEN** `HookRunResult.additional_contexts` contains exactly `"Project is in maintenance mode"`
- **AND** `HookRunResult.outcome` is `HookOutcome::Ok`

#### Scenario: Free-form stdout ignored
- **GIVEN** a hook whose stdout is `"plain text, not json"` (exit 0)
- **WHEN** the runner fires the event
- **THEN** `HookRunResult.additional_contexts` is empty
- **AND** `HookRunResult.outcome` is `HookOutcome::Ok`

#### Scenario: Multiple hooks accumulate in order
- **GIVEN** two hooks emitting `additionalContext` `"one"` then `"two"`
- **WHEN** the runner fires the event
- **THEN** `HookRunResult.additional_contexts` equals `["one", "two"]`

#### Scenario: Block short-circuits later contexts
- **GIVEN** hook A exits 2 and hook B would emit `additionalContext` `"SHOULD NOT RUN"`
- **WHEN** the runner fires the event
- **THEN** `HookRunResult.outcome` is `HookOutcome::Block(…)`
- **AND** `HookRunResult.additional_contexts` is empty

#### Scenario: Query engine injects contexts as user messages
- **GIVEN** a PreToolUse hook that emitted `additionalContext` `"remember to be polite"`
- **WHEN** the query engine runs the next turn
- **THEN** the outgoing message list contains a synthetic user message with that text
- **AND** the message is appended before the next API request is built

### Requirement: Hook Child Process Environment

The runner SHALL export a documented set of environment variables
to every hook child process, derived from a `HookContext` attached
via `HookRunner::with_context`. The set is:

- `CLAUDE_PROJECT_DIR` — workspace root when set.
- `CLAUDE_PLUGIN_ROOT` — plugin (or skill) root dir when set.
- `CLAUDE_PLUGIN_DATA` — writable per-plugin data dir when set.
- `CLAUDE_PLUGIN_OPTION_<KEY>` — one variable per plugin option.
  `<KEY>` is the option name with any non-`[A-Za-z0-9_]` char
  replaced with `_`, then uppercased. This matches TS
  `src/utils/hooks.ts:903-904`.

Unset fields SHALL NOT be exported (the child does not receive an
empty-value variable).

#### Scenario: Plugin option env key normalisation
- **GIVEN** a `HookContext` with plugin_option `"foo-bar" = "v"`
- **WHEN** `HookContext::env_pairs()` is called
- **THEN** the pairs contain `("CLAUDE_PLUGIN_OPTION_FOO_BAR", "v")`

#### Scenario: Child process sees exported env
- **GIVEN** a runner with `HookContext { project_dir=/proj, plugin_root=/plug, plugin_data=/data, plugin_options=[api_token=xyz] }`
- **WHEN** a hook runs `printenv` for each CLAUDE_* var
- **THEN** the child sees all four variables with their configured values

#### Scenario: Empty context exports nothing
- **GIVEN** a default `HookContext`
- **WHEN** `env_pairs()` is called
- **THEN** it returns an empty vector

## ADDED Requirements

### Requirement: HTTP-Hook Header Preparation

`cc_hooks::http::prepare_header(name, value, env, allowlist)` SHALL:

1. Interpolate `${NAME}` and `$NAME` patterns in both arguments
   against `env`, restricted to `allowlist` when provided
   (`None` = any env var).
2. Reject the pair with `HeaderPrepError::UnsetEnvVar` if any
   referenced name is not in `env`.
3. Reject the pair with `HeaderPrepError::DisallowedEnvVar` if any
   referenced name is not in `allowlist` (when an allowlist is set).
4. Reject the pair with `HeaderPrepError::ControlByte` if the
   interpolated name or value contains `\r`, `\n`, or `\0`.

Unterminated `${` without a matching `}` and `$` not followed by
an identifier character SHALL be kept as literal bytes so
ordinary shell-like values (`"cost $100"`) round-trip unchanged.

Failures from `prepare_header` SHALL propagate — no partial
preparation of the header map; the caller MUST NOT ship the
request if any header fails.

#### Scenario: Env var interpolation
- **GIVEN** `env = { TOKEN: "s3cret" }`
- **WHEN** `prepare_header("Authorization", "Bearer $TOKEN", &env, None)` runs
- **THEN** the result is `Ok(("Authorization", "Bearer s3cret"))`

#### Scenario: Unset env var is error
- **GIVEN** empty `env`
- **WHEN** `prepare_header("X-Auth", "$TOKEN", &env, None)` runs
- **THEN** the result is `Err(HeaderPrepError::UnsetEnvVar { name: "TOKEN", .. })`

#### Scenario: CRLF injection blocked in value
- **GIVEN** `env = { INJ: "abc\r\nX-Evil: 1" }`
- **WHEN** `prepare_header("X-Auth", "$INJ", &env, None)` runs
- **THEN** the result is `Err(HeaderPrepError::ControlByte { field: "value", byte: 0x0D })`

#### Scenario: CRLF injection blocked in name
- **GIVEN** `env = { INJ: "Extra\r\nName" }`
- **WHEN** `prepare_header("X-${INJ}", "v", &env, None)` runs
- **THEN** the result is `Err(HeaderPrepError::ControlByte { field: "name", .. })`

#### Scenario: Allowlist miss is error
- **GIVEN** `env = { SECRET: "xyz" }` and `allowlist = ["PUBLIC"]`
- **WHEN** `prepare_header("X-Auth", "Bearer $SECRET", &env, Some(&allowlist))` runs
- **THEN** the result is `Err(HeaderPrepError::DisallowedEnvVar { name: "SECRET", .. })`

#### Scenario: Unterminated dollar kept literal
- **GIVEN** any `env`
- **WHEN** `prepare_header("X", "price: $100", &env, None)` runs
- **THEN** the result is `Ok(("X", "price: $100"))`
