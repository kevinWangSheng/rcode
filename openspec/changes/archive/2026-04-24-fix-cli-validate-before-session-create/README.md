# fix-cli-validate-before-session-create

`claude` creates a fresh session JSONL on disk **before** validating
CLI arguments. A run that fails arg validation (e.g. `--thinking abc`)
still leaves an empty session file in `~/.claude/projects/<...>/`.
Move post-clap validation (currently only `parse_thinking`, but a
slot for any future ones) ahead of `Session::new` / `Session::resume`
so failed runs leave no on-disk trace.
