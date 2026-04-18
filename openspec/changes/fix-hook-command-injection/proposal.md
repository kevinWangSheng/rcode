## Why

`cc-hooks` runs a command hook with:

```rust
Command::new("bash").arg("-c").arg(command_from_config)
```

where `command_from_config` is the raw string from
`~/.claude/settings.json` (or project / local / policy variants). That
string lands directly in a shell. Anything that can write that file —
a settings sync service, a shared-team template import, an earlier
buggy migration, or a future `/config set` slash command — becomes a
code-execution vector.

The settings file is trusted today, but there is no structural reason
it needs to be. The fix is cheap and eliminates an entire class of
future supply-chain risk.

## What Changes

- Accept hooks in two forms:
  1. **Array form** (preferred): `command: ["git", "rev-parse", "HEAD"]`
     spawned via `Command::new(argv[0]).args(&argv[1..])` — no shell.
  2. **String form**: preserved for existing configs, but emits a
     `warn!` on first use and is gated behind an explicit
     `unsafe_shell: true` sibling field on the hook entry.
- Without `unsafe_shell: true`, a string-form `command` is rejected at
  settings-load time with a descriptive error pointing at the field.
- The existing env-variable plumbing (`CLAUDE_SESSION_ID`, etc.) works
  identically for both forms.
- Migration note in `RUST_REWRITE_PLAN.md` and a one-time WARN on
  startup listing any string-form hooks so users can migrate.

## Capabilities

### Modified Capabilities
- `hook-command-execution`: a command hook MUST be expressible as an
  `argv[]` array that executes without an intervening shell. String
  form requires `unsafe_shell: true` opt-in.

## Impact

- **Affected code:** `cc-hooks` command runner, `cc-config` hook
  schema.
- **User-visible:** users with existing string-form hooks get a WARN and
  a clear migration path. New hook authors pick the array form by
  default.
- **Risk:** MEDIUM. Settings schema changes need careful backwards
  compatibility — the `command` field must still accept strings, only
  the execution gate changes.
