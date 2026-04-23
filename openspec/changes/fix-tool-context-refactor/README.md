# fix-tool-context-refactor

Split of `fix-session-resume-wiring` §2 into its own change. Pure
mechanical refactor: introduce `cc_core::ToolContext`, migrate
`Tool::execute` from a bare `CancellationToken` parameter to a
`&ToolContext`, wrap `Session` in `Arc<Session>` inside
`QueryEngine`, and update every tool + call site.

No behaviour change — existing tests must keep passing unmodified
(aside from rewriting call-site mocks). Blocks
`fix-file-history-snapshot-producers`, which needs `ToolContext` to
reach `Session::append_file_history_snapshot` from inside Edit/Write.
