# Spec — CLI short-flag aliases

## ADDED Requirements

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
