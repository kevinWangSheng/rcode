# fix-thinking-engine-wiring

Follow-up to `fix-request-thinking` (which added `ThinkingConfig` +
`CreateMessageRequest::with_thinking`). QA pass on 2026-04-23 showed
`with_thinking` has zero production callers and no config / CLI path
threads a user preference into the request. This change wires the
type end-to-end so extended thinking is actually reachable from the
CLI.
