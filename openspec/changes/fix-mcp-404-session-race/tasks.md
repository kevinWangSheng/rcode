## 1. Session Owner

- [ ] 1.1 Introduce `McpSession { id: RwLock<Option<String>>, version:
      AtomicU64 }` (or equivalent).
- [ ] 1.2 `acquire()` returns `(session_id, version)` snapshot for each
      outbound request.

## 2. 404 Path

- [ ] 2.1 On 404, bump the version, set `id = None`, and schedule a
      reconnect task keyed to the new version.
- [ ] 2.2 Reconnect replays `initialize`; subsequent callers observing
      the new version wait for it, then proceed with the fresh id.

## 3. Retry Policy

- [ ] 3.1 The adapter retries a 404'd request exactly once after the
      reconnect settles. Second 404 (or any other error) bubbles up.
- [ ] 3.2 Distinguish `reqwest::Error::connect` (retry) from
      `reqwest::Error::timeout` (no retry).

## 4. Tests

- [ ] 4.1 Concurrency test: spawn 5 simultaneous `tools/call`, have the
      stub server expire the session, assert all 5 succeed after one
      reconnect.
- [ ] 4.2 Second-404 test: force two consecutive 404s, assert the
      second one surfaces as an error.

## 5. Sign-off

- [ ] 5.1 `cargo test -p cc-mcp` + clippy clean.
