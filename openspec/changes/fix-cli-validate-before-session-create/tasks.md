## 1. Extract validation helper

- [ ] 1.1 Add `struct ParsedCliOptions { thinking: Option<ThinkingConfig> }`
      near the other `Cli`-adjacent types in `rust/cc/src/main.rs`.
      Leave room for future fields.
- [ ] 1.2 Add `fn parse_cli_options(cli: &Cli) -> Result<ParsedCliOptions,
      Box<dyn std::error::Error>>` that calls `parse_thinking` (the
      only current validator) and bundles the result. Pure / no
      I/O / no env reads.

## 2. Reorder `run`

- [ ] 2.1 In `rust/cc/src/main.rs::run`, call `let parsed =
      parse_cli_options(&cli)?;` immediately after the subcommand
      `Login` branch and BEFORE `ensure_fresh_credentials().await?`.
      A failed parse must error out before any disk side effects.
- [ ] 2.2 Replace the existing inline `let thinking = match
      &cli.thinking { ... }` block (currently at ~line 281-284)
      with a single use of `parsed.thinking.clone()` at each
      consumer site (`QueryOptions` build, `BridgeRequest` build).
      Delete the inline match.
- [ ] 2.3 Add a comment above the new call site noting "validation
      goes first so failures don't leak session files".

## 3. Tests

- [ ] 3.1 Add `parse_cli_options_rejects_invalid_thinking` in
      `cc/src/main.rs::tests` per the proposal §3 snippet. Asserts
      `Err` with a message containing "--thinking".
- [ ] 3.2 Add `parse_cli_options_accepts_valid_inputs` covering
      (a) `--thinking adaptive` → `Some(Adaptive)`, (b) no
      `--thinking` → `None`, (c) `--thinking 2048` → `Some(Enabled
      { 2048 })`.
- [ ] 3.3 Optional: end-to-end smoke shell test in
      `cc/tests/print_e2e.rs` (or wherever) confirming a failed
      `--thinking abc` invocation creates zero new files under a
      tempdir-scoped `CLAUDE_HOME` (only if a clean way to redirect
      session storage already exists; otherwise note as deferred).

## 4. Verification

- [ ] 4.1 `cargo fmt -p claude-cli` clean.
- [ ] 4.2 `cargo clippy -p claude-cli --all-targets -- -D warnings`
      clean.
- [ ] 4.3 `cargo test -p claude-cli` — all green, including new
      tests.
- [ ] 4.4 Manual smoke: count `*.jsonl` under
      `~/.claude/projects/<workspace>/` before and after running
      `cargo run --bin claude -- --thinking abc --print hi`. The
      count MUST be identical (no leak). Confirm no `Session:
      <uuid>` line is printed.

## 5. Sign-off

- [ ] 5.1 Commit references the smoke pass that surfaced the gap
      (npcterm session 2026-04-23, HEAD `8bbeac1`).
