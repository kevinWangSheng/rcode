# Spec — CLI startup ordering

## ADDED Requirements

### Requirement: Input validation precedes session creation

`claude` MUST validate every CLI argument that can reject user
input (today: `--thinking`; tomorrow: any added validators) BEFORE
either creating a new session or resuming an existing one. A run
that fails validation MUST NOT:

- create or write to any session JSONL file under
  `~/.claude/projects/<workspace>/`,
- print a `Session: <uuid>` line on stderr,
- modify any auth credential file.

Successful validation MUST proceed exactly as today — same session
creation, same printed session id, same downstream behaviour.

#### Scenario: invalid `--thinking` value leaves no on-disk trace

- **Given** the count of `*.jsonl` files under
  `~/.claude/projects/<workspace>/` is N
- **When** the user runs `claude --thinking abc --print hi`
- **Then** the command exits non-zero with the canonical
  `--thinking: expected integer, 'adaptive', or 'off', got "abc"`
  message
- **And** the count of `*.jsonl` files under that directory is
  still N
- **And** stderr does NOT contain the substring `Session: `

#### Scenario: valid `--thinking` value still creates the session

- **Given** the user runs `claude --thinking adaptive --print "hi"`
- **When** parse succeeds
- **Then** the run proceeds as today: `Session: <uuid>` is printed,
  a new JSONL file is created, and the API request body carries
  `{"thinking":{"type":"adaptive"}}`

### Requirement: Validation helper is side-effect-free

The `parse_cli_options(cli)` helper MUST NOT perform disk I/O,
network calls, or environment-variable reads beyond clap's
already-parsed `Cli` struct. Any future validator added to it MUST
be similarly pure so the "validate before disk" guarantee stays
intact.
