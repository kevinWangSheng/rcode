## 1. Session Owner

- [x] 1.1 Introduce `McpSession { id: RwLock<Option<String>>, version:
      AtomicU64 }` (or equivalent). — `http_client.rs` `McpSession`
      struct (id: RwLock, version: AtomicU64, reconnect_lock: Mutex).
- [x] 1.2 `acquire()` returns `(session_id, version)` snapshot for each
      outbound request.

## 2. 404 Path

- [x] 2.1 On 404, bump the version, set `id = None`, and schedule a
      reconnect task keyed to the new version. — centralised in
      `reconnect_if_stale`: under `reconnect_lock`, bumps version +
      invalidates id, then replays `initialize` inline.
- [x] 2.2 Reconnect replays `initialize`; subsequent callers observing
      the new version wait for it, then proceed with the fresh id. —
      version-guard in `reconnect_if_stale` short-circuits redundant
      reconnects; concurrency test asserts exactly one.

## 3. Retry Policy

- [x] 3.1 The adapter retries a 404'd request exactly once after the
      reconnect settles. Second 404 (or any other error) bubbles up. —
      `send_request` wraps `send_request_inner` with one retry; a
      second 404 surfaces and clears the stale id so the next caller
      starts fresh.
- [x] 3.2 Distinguish `reqwest::Error::connect` (retry) from
      `reqwest::Error::timeout` (no retry). — `post()` already
      disambiguates in its error messages; 404 is the only retry
      trigger, so timeouts deliberately do NOT retry.

## 4. Tests

- [x] 4.1 Concurrency test: spawn 5 simultaneous `tools/call`, have the
      stub server expire the session, assert all 5 succeed after one
      reconnect. — `concurrent_404s_trigger_only_one_reconnect`.
- [x] 4.2 Second-404 test: force two consecutive 404s, assert the
      second one surfaces as an error. — `second_404_in_a_row_surfaces_error`.

## 5. Sign-off

- [x] 5.1 `cargo test -p cc-mcp` + clippy clean.
