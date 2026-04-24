## 1. Add the short flag

- [x] 1.1 In `rust/cc/src/main.rs`, change the `print` field's
      attribute from `#[arg(long, value_name = "TEXT")]` to
      `#[arg(short = 'p', long, value_name = "TEXT")]`.
- [x] 1.2 Confirm `-p` doesn't collide: `grep "short = 'p'"
      rust/cc/src/main.rs` returns one hit (the new line) and no
      others. (`-m` / `-v` / `-h` / `-V` are the existing shorts;
      `-p` is free.)

## 2. Test

- [x] 2.1 Add `print_short_flag_p_equivalent_to_long` in
      `rust/cc/src/main.rs::tests` per the proposal §3 snippet.
      Asserts both spellings parse to `print: Some("hello".into())`.

## 3. Verification

- [x] 3.1 `cargo fmt -p claude-cli` clean.
- [x] 3.2 `cargo clippy -p claude-cli --all-targets -- -D warnings`
      clean.
- [x] 3.3 `cargo test -p claude-cli` — new test green; existing 9
      claude-cli tests still pass.
- [x] 3.4 Smoke: `cargo run --bin claude -- -p` now errors with
      `a value is required for '--print <TEXT>'` (clap-level), not
      the former `unexpected argument '-p' found`. Proves `-p` is
      recognised as the short alias. (Original `-p hi --thinking
      abc` variant would also work but hits credential-fetch first
      on dev machines; the zero-value check is equivalent and needs
      no auth.)

## 4. Sign-off

- [x] 4.1 Commit references the smoke pass that surfaced this gap.
