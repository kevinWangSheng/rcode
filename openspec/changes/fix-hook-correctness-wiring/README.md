# fix-hook-correctness-wiring

Follow-up to `fix-hook-correctness` covering the two cross-crate
consumer-wiring items the QA pass on 2026-04-23 flagged as unfinished:

1. cc-query `PreToolUse` must consume `HookRunResult.
   additional_contexts` and inject them as user messages on the next
   turn. Today they are silently discarded.
2. cc-query `tool_use` path must consume
   `HookRunResult.async_rewake`. Today the field is set but never
   read; AsyncRewake hooks behave identically to Block hooks.

Also closes the schema bug (`async_rewake` missing serde alias) and
the 5 missing tests (7.1 / 7.3 / 7.8 / 7.9 / 7.10) noted in the
parent change's `tasks.md`.
