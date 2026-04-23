# fix-engine-stream-mock-harness

Tiny test-only change. Adds a scriptable stream-producer override on
`cc_query::engine::QueryEngine` so full `run_turn` flows can be
driven end-to-end without hitting a live API. Unblocks three
previously deferred tests across two other changes:

1. `fix-hook-correctness-wiring` §5.3 —
   `additional_contexts_are_cleared_on_cancel`.
2. `fix-file-history-snapshot-producers` cancel-path tests
   (`cancel_during_tool_use_appends_canonical_markers` and
   `cancel_without_tool_use_appends_plain_marker`).
3. Future `fix-cache-control-engine-wiring` integration test
   (`request_body_carries_ephemeral_on_last_block`) also benefits
   — it today captures the serialised request, not a full turn,
   but will want this harness when it grows to assert behaviour
   over multiple iterations.

Independent of `fix-tool-context-refactor`. Can land in either
order.
