## 1. Add the two short flags

- [x] 1.1 In `rust/cc/src/main.rs`, change the `r#continue` field's
      attribute from `#[arg(long)]` to `#[arg(short = 'c', long)]`.
- [x] 1.2 In the same file, change the `resume` field's attribute
      from `#[arg(long, value_name = "SESSION_ID")]` to
      `#[arg(short = 'r', long, value_name = "SESSION_ID")]`.
- [x] 1.3 Confirm neither letter collides: `grep "short = 'c'"
      rust/cc/src/main.rs` returns exactly one hit (the new line),
      same for `short = 'r'`. Existing shorts are `-m` / `-p` / `-v`
      per the proposal table — both `-c` and `-r` are free.

## 2. Tests

- [x] 2.1 Add `continue_short_flag_c_equivalent_to_long` in
      `cc/src/main.rs::tests` per the proposal §2 snippet. Asserts
      both `--continue` and `-c` parse to `r#continue == true`.
- [x] 2.2 Add `resume_short_flag_r_equivalent_to_long` in the same
      module. Asserts both `--resume sess-abc` and `-r sess-abc`
      parse to `resume: Some("sess-abc")`.

## 3. Verification

- [x] 3.1 `cargo fmt -p claude-cli` clean.
- [x] 3.2 `cargo clippy -p claude-cli --all-targets -- -D warnings`
      clean.
- [x] 3.3 `cargo test -p claude-cli` — the 2 new tests pass; the
      existing 12 tests (10 before + 2 from the fix-cli changes)
      still pass.
- [x] 3.4 Smoke: `cargo run --bin claude -- -c` now errors with
      a credential or session-lookup message (not `unexpected
      argument '-c' found`), proving `-c` reaches parse stage.
      `cargo run --bin claude -- -r` errors with `a value is
      required for '--resume <SESSION_ID>'` at clap level,
      proving `-r` is recognised.

## 4. Sign-off

- [x] 4.1 Commit references this being a follow-up to
      `fix-cli-print-short-flag` (commit `c197fb5`).
- [x] 4.2 Update `fix-cli-print-short-flag`'s open-question §1 to
      point at this change as the resolution.
- [ ] 4.3 Post-merge, archive via
      `npx @fission-ai/openspec archive fix-cli-short-aliases`.
