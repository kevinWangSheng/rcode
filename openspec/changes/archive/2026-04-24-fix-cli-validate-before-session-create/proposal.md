# Proposal — Validate CLI args before creating the session

## Why

The 2026-04-23 npcterm smoke pass on `phase3/implementation` HEAD
`8bbeac1` ran:

```
$ claude --thinking abc --print hi
Session: d62c43a4-24f2-4a7b-9243-42add85c9a54
error: --thinking: expected integer, 'adaptive', or 'off', got "abc"
```

The `Session: <uuid>` line is `eprintln!`'d from `Session::new` (or
the resume path), and that runs at `cc/src/main.rs:168-176` —
**before** `parse_thinking` runs at `cc/src/main.rs:281-284`. So a
parse failure leaves behind an empty / unused JSONL file in
`~/.claude/projects/<workspace>/<uuid>.jsonl` plus a printed session
id the user never had a chance to use.

This is mostly a hygiene issue: the orphan files accumulate over time
and `--continue` will eventually pick one up that has zero useful
content. Behaviour-wise it's harmless; UX-wise it's a "why is this
session empty?" puzzle for users who hit a typo.

This isn't a regression of any specific batch — `parse_thinking` is
the first post-clap validator that needed to fail loudly, and it
exposed the latent ordering. Future validators (env-var checks,
config sanity, etc.) will hit the same trap.

## Goal

Reorder `cc/src/main.rs::run` so every "can fail to validate user
input" step happens **before** `Session::new` / `Session::resume`.
Failed runs MUST leave no JSONL on disk and MUST NOT print a
`Session: …` line.

Not in scope:

- Any change to session JSONL format / location.
- Cleaning up existing orphan session files on disk. The user can
  run a one-off `find ~/.claude/projects -name '*.jsonl' -size -2c
  -delete` if they want to reclaim the leaked files; this change
  only prevents future leaks.
- Refactoring `run` into smaller functions. Keep edits minimal.

## What changes

### 1. New helper `parse_cli_options(cli, max_tokens)`

In `rust/cc/src/main.rs`, extract the post-clap validation into a
small function that returns either a populated `CliOptions` struct
(or just a tuple) or an `Err` propagated up:

```rust
struct ParsedCliOptions {
    thinking: Option<ThinkingConfig>,
    // future: validated env vars, config overrides, etc.
}

fn parse_cli_options(cli: &Cli) -> Result<ParsedCliOptions, Box<dyn std::error::Error>> {
    let thinking = match &cli.thinking {
        Some(raw) => Some(parse_thinking(raw, cli.max_tokens)?),
        None => None,
    };
    Ok(ParsedCliOptions { thinking })
}
```

Add new validators here as they appear. The whole point is "anything
that can return `Err(...)` from CLI input lives in this one
function".

### 2. Reorder `run` to call validation first

The current ordering at `rust/cc/src/main.rs::run`:

```
1. Subcommand branches (Login)
2. ensure_fresh_credentials                ← reads disk, can fail
3. load_settings                           ← reads disk, can fail
4. resolve_model
5. atty / print / interactive_tui decisions
6. resolve_id (--continue → max session id) ← reads disk
7. Session::new OR Session::resume          ← *** WRITES TO DISK ***
8. ... lots of other setup ...
9. parse_thinking                          ← *** validates user input ***
```

Target ordering:

```
1. Subcommand branches (Login)
2. parse_cli_options(&cli)                  ← *** moved up ***
3. ensure_fresh_credentials
4. load_settings
5. resolve_model
6. atty / print / interactive_tui decisions
7. resolve_id (--continue)
8. Session::new OR Session::resume          ← only reached on success
9. ... rest unchanged ...
```

`parse_cli_options` runs before `ensure_fresh_credentials` because
auth is a side-effecty disk read too (it can also create token files
on first OAuth login). Push purely-deterministic input validation
all the way up.

The downstream caller code that today does:

```rust
let thinking = match &cli.thinking {
    Some(raw) => Some(parse_thinking(raw, cli.max_tokens)?),
    None => None,
};
let options = QueryOptions { ..., thinking: thinking.clone() };
```

becomes:

```rust
let options = QueryOptions { ..., thinking: parsed.thinking.clone() };
```

`parsed` is the `ParsedCliOptions` from step 2.

### 3. Smoke verification

After the change, this command must NOT create a session file or
print `Session: <uuid>`:

```
$ claude --thinking abc --print hi
error: --thinking: expected integer, 'adaptive', or 'off', got "abc"
$ ls ~/.claude/projects/<workspace>/ | wc -l    # unchanged from before
```

Add an assertion test that exercises the parse path without touching
disk:

```rust
#[test]
fn invalid_thinking_returns_err_without_session_create() {
    // parse_cli_options is pure — no disk side effects. Just confirm
    // it errors out so the rest of run() never gets to Session::new.
    let cli = Cli::try_parse_from(["claude", "--thinking", "abc"]).unwrap();
    let result = parse_cli_options(&cli);
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("--thinking"));
}
```

## Impact

- **Affected specs**: new `cli-startup-order` cap with two
  scenarios.
- **Affected crates**: `cc` (claude binary) only.
- **Behaviour compatibility**: strictly an improvement. Successful
  runs are unchanged. Failed validation runs no longer leak session
  files or print misleading session ids.
- **Risk**: low. The only reordering risk is if some downstream code
  expected a side-effect from `Session::new` / OAuth refresh /
  config load to happen before validation. None do today; any
  future validator added to `parse_cli_options` should be
  side-effect-free by construction.

## Open questions

1. Should `ensure_fresh_credentials` move under `parse_cli_options`
   too? Probably not — it's not strictly "input validation", it's a
   prereq for the run. Keep it at step 3 for now.
2. The existing `Session: <uuid>` `eprintln!` is the actual bug
   surface. Could be silenced when `--print` is set, but that's a
   separate UX call. Leave for follow-up if anyone asks.
