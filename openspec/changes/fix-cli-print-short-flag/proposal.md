# Proposal — Add `-p` short alias for `--print`

## Why

The 2026-04-23 npcterm smoke pass on `phase3/implementation` HEAD
`8bbeac1` ran `$CLAUDE -p hi` and got:

```
error: unexpected argument '-p' found

Usage: claude [OPTIONS] [COMMAND]
For more information, try '--help'.
```

The TS reference accepts `-p` as a short alias for `--print` (SDK /
non-interactive single-shot). The Rust CLI declares the field as
`#[arg(long, value_name = "TEXT")] pub print: Option<String>` — no
`short = 'p'`. Users (and CI scripts) reaching for the short form
hit a confusing parse failure even though `--print hi` works fine.

This is a one-line ergonomics gap surfaced during smoke-testing the
batch A/B/C wiring landings; not a regression of any of those
changes.

## Goal

Make `claude -p TEXT` behave identically to `claude --print TEXT`.

Not in scope:

- Rest of the CLI's flags. Only `--print` has a documented TS short
  alias that's missing in Rust. If other shorts are missing too,
  open separate changes per flag (or one combined `fix-cli-short-
  aliases` change auditing all of them; not this scope).
- Behaviour of `--print` itself. The flag does what the comment says
  it does; only the surface gets a second spelling.

## What changes

### 1. Add the short flag in `rust/cc/src/main.rs`

```rust
// Cli struct, around line 40-41
-    /// SDK / non-interactive mode: send a single prompt, print the response,
-    /// exit with code 0. Equivalent to `--message <TEXT> --no-tui --non-interactive`.
-    #[arg(long, value_name = "TEXT")]
-    print: Option<String>,
+    /// SDK / non-interactive mode: send a single prompt, print the response,
+    /// exit with code 0. Equivalent to `--message <TEXT> --no-tui --non-interactive`.
+    #[arg(short = 'p', long, value_name = "TEXT")]
+    print: Option<String>,
```

`-p` is a free letter — `--permissions` is not a thing, the closest
existing short is `-m` for `--message`, so no collision. Verify with
`grep "short = 'p'" rust/cc/src/main.rs` — should return zero hits
before the change.

### 2. Verify in smoke

Smoke check (no API call needed if the prompt would error first):

```
cargo run --bin claude -- -p hi --thinking abc
```

Expected output: the same `error: --thinking: expected integer,
'adaptive', or 'off', got "abc"` that `--print hi --thinking abc`
produces today. If the parser still rejects `-p`, the change is
incomplete.

### 3. Tests

Add a clap-level smoke test in `cc/src/main.rs::tests`:

```rust
#[test]
fn print_short_flag_p_equivalent_to_long() {
    use clap::Parser;
    let long = Cli::parse_from(["claude", "--print", "hello"]);
    let short = Cli::parse_from(["claude", "-p", "hello"]);
    assert_eq!(long.print, Some("hello".into()));
    assert_eq!(short.print, Some("hello".into()));
    assert_eq!(long.print, short.print);
}
```

That's enough to lock the alias in; the surrounding behaviour is
already covered by existing `--print` integration paths in cc-bridge.

## Impact

- **Affected specs**: new `cli-flags` cap with one scenario. No
  existing spec changes.
- **Affected crates**: `cc` (claude binary) only.
- **Wire / behaviour compatibility**: additive. `-p` previously
  errored; now it works. Long form unchanged.

## Open questions

1. Audit the rest of the CLI for other missing short flags TS has?
   (`-c` for `--continue`, `-r` for `--resume`, etc.) Out of scope
   for this one-liner; spin up a second change if you want the full
   sweep.
