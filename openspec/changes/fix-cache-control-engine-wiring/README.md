# fix-cache-control-engine-wiring

Follow-up to `fix-content-block-cache-control` (which added the field)
and the 2026-04-23 QA pass that showed the helper has no production
callers. This change restores end-to-end prompt-cache parity by
tagging the trailing block of the trailing message inside
`QueryEngine::run_turn`.
