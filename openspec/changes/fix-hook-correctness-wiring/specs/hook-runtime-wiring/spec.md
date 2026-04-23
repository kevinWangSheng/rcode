# Spec — Hook-runtime consumer wiring

## ADDED Requirements

### Requirement: HookConfig.async_rewake accepts snake- and camelCase JSON

The `HookConfig.async_rewake` field SHALL be decodable from
settings files using either spelling: `async_rewake` (Rust /
settings convention) or `asyncRewake` (TS convention). The
deserialiser SHALL accept both via `#[serde(default, alias =
"asyncRewake")]` on the field.

#### Scenario: snake_case JSON still parses

- **Given** `{"async_rewake": true, "type": "command", "command":
  "true"}`
- **When** parsed as `HookConfig`
- **Then** `async_rewake == true`

#### Scenario: camelCase JSON parses

- **Given** `{"asyncRewake": true, "type": "command", "command":
  "true"}`
- **When** parsed as `HookConfig`
- **Then** `async_rewake == true`

### Requirement: cc-query drains PreToolUse additional_contexts into the next turn

`cc_query::engine::QueryEngine` SHALL carry a
`pending_additional_contexts: Vec<String>` field. On every
`run_turn` loop iteration, before building the
`CreateMessageRequest`, the engine SHALL drain this vector and
append each string as a `MessageParam::user` entry (both to the
session via `session.append` and to the in-memory `messages`
vector).

The PreToolUse hook site SHALL call
`pending_additional_contexts.extend(hook_result.
additional_contexts.clone())` immediately after the hook runs
— before the `blocked` check — so contexts propagate even if a
later hook in the chain blocks.

The engine SHALL clear `pending_additional_contexts` before
returning `Err(...)` from `run_turn` to avoid cross-turn leaks in
reused engines.

#### Scenario: PreToolUse context reaches the next turn's request body

- **Given** a PreToolUse hook that emits
  `additionalContext: ["REMEMBER: X"]` and does not block
- **And** the tool call itself succeeds
- **When** the engine runs the next iteration of the loop
- **Then** the new `CreateMessageRequest` body has a user message
  `"REMEMBER: X"` as one of the leading entries
- **And** the `pending_additional_contexts` vector is now empty

#### Scenario: Two contexts from two hooks arrive in order

- **Given** two PreToolUse hooks emitting `"A"` and `"B"` in that
  order
- **When** the next turn's request body is built
- **Then** the user messages `"A"` and `"B"` appear in that order

#### Scenario: Error-exit clears the vector

- **Given** `pending_additional_contexts == ["X"]` (somehow
  populated)
- **When** `run_turn` returns `Err(CcError::Cancelled)` via the
  cancel path
- **Then** a subsequent call to `run_turn` observes
  `pending_additional_contexts == []` on entry

### Requirement: cc-query surfaces AsyncRewake as a distinct tool_result error

The cc-query engine MUST distinguish AsyncRewake from Block in the
tool-result error it surfaces to the caller, so logs and downstream
consumers can tell the two delivery paths apart. Specifically, when a
PreToolUse hook's `HookRunResult.async_rewake` is `Some(msg)`, the
engine's tool-execution path SHALL return
`Err(tool_result_error(tu.id, "Hook requested async rewake:
<msg>"))` instead of the plain Block error. The prefix
`"Hook requested async rewake: "` MUST be present so consumers (and
logs) can distinguish the two delivery paths.

A follow-up change ("Batch G task-notification-queue") SHALL
replace this early-return with a queued mid-query re-entry; this
change ships a `TODO` comment at the call site pointing at that
future batch.

#### Scenario: AsyncRewake message reaches the caller

- **Given** a PreToolUse hook that exits 2 with `async_rewake: true`
  and stdout `"please rewake"`
- **When** the tool-call handler runs
- **Then** the handler returns `Err(ToolResultBlock)` whose
  content string contains both `"Hook requested async rewake:"`
  and `"please rewake"`

#### Scenario: Block and AsyncRewake differ in delivery

- **Given** two stub hooks: Hook A blocks with message `"no"`,
  Hook B async-rewakes with message `"no"`
- **When** both are run in separate engine invocations
- **Then** Hook A's tool_result error does NOT contain the prefix
  `"Hook requested async rewake:"`
- **And** Hook B's tool_result error DOES contain the prefix

### Requirement: `HookContext::env_pairs` emits deterministic output

`HookContext::env_pairs()` SHALL return `(key, value)` pairs in
this fixed order when set:

1. `CLAUDE_PROJECT_DIR`
2. `CLAUDE_PLUGIN_ROOT`
3. `CLAUDE_PLUGIN_DATA`
4. Zero or more `CLAUDE_PLUGIN_OPTION_<KEY>` entries, sorted
   lexicographically by the normalised `<KEY>`.

Unset optional fields (project_dir / plugin_root / plugin_data)
SHALL NOT appear in the result. A default (empty)
`HookContext::env_pairs()` SHALL return an empty vector.

#### Scenario: Full context emits all five keys in order

- **Given** `HookContext { project_dir: Some("/p"), plugin_root:
  Some("/pr"), plugin_data: Some("/pd"), plugin_options:
  {"zeta": "z", "alpha": "a"} }`
- **When** `env_pairs()` runs
- **Then** the result is
  `[("CLAUDE_PROJECT_DIR", "/p"), ("CLAUDE_PLUGIN_ROOT", "/pr"),
    ("CLAUDE_PLUGIN_DATA", "/pd"),
    ("CLAUDE_PLUGIN_OPTION_ALPHA", "a"),
    ("CLAUDE_PLUGIN_OPTION_ZETA", "z")]`

#### Scenario: Empty context emits nothing

- **Given** `HookContext::default()`
- **When** `env_pairs()` runs
- **Then** the result is an empty vec

### Requirement: Hook child process sees plugin env vars end-to-end

A hook command launched via `HookRunner::with_context` SHALL see
the pairs from `HookContext::env_pairs()` injected into its
environment. A regression test SHALL exercise this via a bash
hook that echoes the values of both
`$CLAUDE_PROJECT_DIR` and `$CLAUDE_PLUGIN_OPTION_FOO` inside a
JSON `hookSpecificOutput.additionalContext`, letting the test
assert the values round-trip through the child process.

#### Scenario: Bash hook reads and echoes plugin env

- **Given** `HookRunner` built with `HookContext { project_dir:
  Some("/tmp/p"), plugin_options: {"foo": "bar"}, ... }`
- **And** a hook with command
  `bash -c 'printf "{\"hookSpecificOutput\":{\"additionalContext\":
  \"%s|%s\"}}" "$CLAUDE_PROJECT_DIR" "$CLAUDE_PLUGIN_OPTION_FOO"'`
- **When** the hook runs
- **Then** `HookRunResult.additional_contexts` contains
  `"/tmp/p|bar"`
