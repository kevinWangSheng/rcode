# fix-cli-short-aliases

Follow-up to `fix-cli-print-short-flag`: audit the Rust CLI for short
flags the TS reference exposes but Rust doesn't. Adds `-c` for
`--continue` and `-r` for `--resume` (the two clean additive gaps).
The `-v` / `-n` / `-w` / `-d` divergences are surveyed and documented
but held out of scope — they either collide with existing Rust
shorts (`-v` = `--verbose`, changing it is a breaking UX call) or
apply to TS flags Rust doesn't implement yet (`--name`, `--worktree`,
`--debug`).

Surfaced as the open-question in `fix-cli-print-short-flag`'s
proposal; `$CLAUDE -c` and `$CLAUDE -r <id>` both fail today with
`unexpected argument` even though TS has supported them since the
original CLI.
