# fix-cli-print-short-flag

Add the `-p` short alias to `--print` on `claude` so SDK / non-
interactive invocations match the TS CLI shape (`claude -p "say hi"`).
Surfaced by the 2026-04-23 npcterm smoke pass: `$CLAUDE -p hi`
errored with `unexpected argument '-p' found` because the field is
declared `#[arg(long, ...)]` only.
