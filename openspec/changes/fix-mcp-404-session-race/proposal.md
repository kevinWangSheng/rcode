## Why

`cc-mcp/src/http_client.rs:290-303` handles Streamable-HTTP session
expiry like this:

- Send the request with the current `session_id`.
- If the server returns 404, clear `session_id` under a mutex.
- Return the 404 as an error to the caller.

Two things break under real load:

1. **Concurrent in-flight race.** Another request captured the old
   `session_id` *before* the 404 cleared it. That request is now flying
   with a stale ID. When it lands it also gets a 404. Clients see a
   burst of 404s on every expiry event, one per concurrent call.
2. **No auto-reconnect.** Clearing the ID is not enough. The caller
   must itself re-run `initialize`, redo `tools/list` if it cached, then
   retry — and nothing in the adapter layer does that. In practice the
   tool fails, the model retries, and with bad timing we loop.

## What Changes

- Introduce an `McpSession` owner with a version counter. Every request
  captures `(session_id, version)`. A 404 increments the version and
  schedules a reconnect; the reconnect is exactly-once per version
  bump (a `tokio::sync::OnceCell`-style primitive under the mutex).
- On receiving 404 for version `v`, callers await the reconnect for
  version `v+1` and retry the request exactly once. If the retry also
  fails, bubble the error — we don't want infinite loops.
- The reconnect replays `initialize` and keeps the connection's
  `tools/list` cache consistent.
- Distinguish timeouts from connect errors in the retry policy: a
  connect error retries (network flap); a request timeout does not
  retry (the server was reached, it just was slow).

## Capabilities

### Modified Capabilities
- `mcp-http-session`: MCP HTTP transport MUST auto-reconnect and retry
  exactly once on 404 session expiry, without user intervention.

## Impact

- **Affected code:** `cc-mcp/src/http_client.rs`, possibly `lib.rs`
  around the transport trait.
- **Risk:** MEDIUM. Reconnect semantics are subtle; concurrent tests are
  essential.
