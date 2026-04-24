## 1. Add the short flag

- [ ] 1.1 In `rust/cc/src/main.rs`, change the `print` field's
      attribute from `#[arg(long, value_name = "TEXT")]` to
      `#[arg(short = 'p', long, value_name = "TEXT")]`.
- [ ] 1.2 Confirm `-p` doesn't collide: `grep "short = 'p'"
      rust/cc/src/main.rs` returns one hit (the new line) and no
      others. (`-m` / `-v` / `-h` / `-V` are the existing shorts;
      `-p` is free.)

## 2. Test

- [ ] 2.1 Add `print_short_flag_p_equivalent_to_long` in
      `rust/cc/src/main.rs::tests` per the proposal §3 snippet.
      Asserts both spellings parse to `print: Some("hello".into())`.

## 3. Verification

- [ ] 3.1 `cargo fmt -p claude-cli` clean.
- [ ] 3.2 `cargo clippy -p claude-cli --all-targets -- -D warnings`
      clean.
- [ ] 3.3 `cargo test -p claude-cli` — new test green; existing 9
      claude-cli tests still pass.
- [ ] 3.4 Smoke: `cargo run --bin claude -- -p hi --thinking abc`
      errors with `--thinking: expected integer ...` (proves `-p`
      reached parse stage; --thinking blocks the API call).

## 4. Sign-off

- [ ] 4.1 Commit references the smoke pass that surfaced this gap.
