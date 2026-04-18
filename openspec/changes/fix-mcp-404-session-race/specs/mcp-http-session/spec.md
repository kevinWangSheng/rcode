## ADDED Requirements

### Requirement: Auto-reconnect on Session Expiry

The MCP HTTP transport SHALL, on receiving a 404 response indicating
session expiry, transparently re-initialize the session and retry the
failing request exactly once before surfacing the error to the caller.

Concurrent in-flight requests sharing the same expired session SHALL
coalesce on a single reconnect: only one `initialize` is issued per
version bump, and all waiters proceed once it completes.

The transport SHALL distinguish connect errors (retryable) from request
timeouts (not retryable).

#### Scenario: Single request, expired session
- **GIVEN** a session that has expired server-side
- **WHEN** a `tools/call` is issued
- **THEN** the transport receives the 404, calls `initialize`, and
  retries the `tools/call`
- **AND** the caller observes a successful response

#### Scenario: Five concurrent requests, expired session
- **GIVEN** five `tools/call` requests flying concurrently against an
  expired session
- **WHEN** all five receive 404s
- **THEN** exactly one `initialize` is sent
- **AND** all five calls complete successfully after the reconnect

#### Scenario: Second 404 bubbles
- **GIVEN** a server that returns 404 even after a fresh initialize
- **WHEN** the transport retries once
- **THEN** the second 404 is surfaced as an error to the caller
  (no infinite retry)
