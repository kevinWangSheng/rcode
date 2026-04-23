# fix-session-resume-wiring

Narrowed 2026-04-23 to the interrupt-marker slice only. The
file-history snapshot wiring, the `Tool::execute` signature
migration, and the cancel-path regression tests have moved to
three dedicated follow-ups:

- `fix-tool-context-refactor`
- `fix-engine-stream-mock-harness`
- `fix-file-history-snapshot-producers`

Archive this change after
`fix-file-history-snapshot-producers` lands.
