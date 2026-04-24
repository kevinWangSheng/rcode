# cli-flags Specification

## Purpose

Tracks the top-level CLI short-flag aliases the `claude` binary
SHALL accept to match the TS reference. Each requirement pins one
short letter to its long form and the scenarios lock in
alias-equivalence at the clap-parse layer.

## Requirements
### Requirement: `-p` is a short alias for `--print`

The `claude` binary SHALL accept `-p TEXT` as a short alias for
`--print TEXT`. Both spellings MUST produce the same parsed
`Option<String>` value, and downstream behaviour (SDK headless
single-shot path) MUST be identical.

#### Scenario: short and long parse identically

- **Given** invocations `claude --print "hello"` and `claude -p
  "hello"`
- **When** clap parses each
- **Then** both yield `Cli.print == Some("hello".into())`

#### Scenario: short flag participates in normal arg ordering

- **Given** `claude -p hi --thinking abc`
- **When** the binary runs
- **Then** parsing succeeds for both flags; the run-time validation
  in `parse_thinking` rejects `"abc"` with the same error string
  the long form would produce
- **And** the failure mode is identical to `claude --print hi
  --thinking abc`

### Requirement: `-c` is a short alias for `--continue`

The `claude` binary SHALL accept `-c` as a short alias for
`--continue`. Both spellings MUST produce the same parsed bool value
and downstream behaviour ("resume most recent session in this
workspace").

#### Scenario: short and long parse identically

- **Given** invocations `claude --continue` and `claude -c`
- **When** clap parses each
- **Then** both yield `Cli.r#continue == true`

### Requirement: `-r` is a short alias for `--resume`

The `claude` binary SHALL accept `-r <SESSION_ID>` as a short alias
for `--resume <SESSION_ID>`. Both spellings MUST produce the same
parsed `Option<String>` value and the same resume-by-id behaviour.

#### Scenario: short and long parse identically

- **Given** invocations `claude --resume sess-abc` and `claude -r
  sess-abc`
- **When** clap parses each
- **Then** both yield `Cli.resume == Some("sess-abc".into())`

#### Scenario: bare `-r` requires a value (parity gap noted)

- **Given** `claude -r` (no trailing token)
- **When** clap parses
- **Then** parsing fails with `a value is required for '--resume
  <SESSION_ID>'`
- **And** this behaviour is IDENTICAL to the existing `claude
  --resume` (no value) failure — the short alias does NOT introduce
  new divergence relative to the long form

Note: TS's `--resume [value]` accepts bare `-r` (opens an
interactive picker). That value-optional behaviour is tracked as a
separate parity gap and is NOT in scope for this requirement; the
short alias mirrors Rust's existing long-form contract.

