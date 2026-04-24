# fix-file-history-snapshot-producers

Split of `fix-session-resume-wiring` §3 / §4 / §6 into its own
change. Wires Edit and Write tools to call
`SessionSink::append_file_history_snapshot` on successful mutation
of pre-existing files, closing P0 #6 end-to-end. Also adds the
cancel-path regression tests that lock in the already-landed §1
interrupt-marker behaviour.

**Depends on**:

- `fix-tool-context-refactor` — for `ToolContext.session:
  Arc<dyn SessionSink>` reaching the tool body.
- `fix-engine-stream-mock-harness` — for the two cancel-path
  regression tests in §6.

Both upstream changes can land independently; this one blocks on
both.
