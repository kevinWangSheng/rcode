# fix-session-resume-integrity

Make `--resume` actually rebuild a faithful session: deserialise the full
JSONL entry union (not just user/assistant turns), persist the
interrupt marker + file-history snapshots TS writes, and pipe resumed
messages into the headless query engine instead of erroring out.
