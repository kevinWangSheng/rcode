# fix-session-resume-wiring

Follow-up to `fix-session-resume-integrity` (which ported the TS
`SessionEntry` meta union, `INTERRUPT_MESSAGE*` consts, and the
`FileHistorySnapshot` types). QA on 2026-04-23 verified all types +
append methods are correct and tested in isolation, **but the two
new append methods have zero production callers** — so resumed
sessions still lose the same data the fix was supposed to preserve.

Scope: wire `append_interrupt_marker` and
`append_file_history_snapshot` into the producers that should be
calling them today (cc-query cancel path; Edit/Write tools). Out of
scope: persisting the richer meta variants like `tag`, `pr-link`,
`mode` etc. — those are written by higher-level user actions that
don't yet exist in the Rust rewrite.
