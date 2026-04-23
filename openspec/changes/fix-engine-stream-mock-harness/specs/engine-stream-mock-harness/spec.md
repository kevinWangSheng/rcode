# Spec — Engine stream-producer override for tests

## ADDED Requirements

### Requirement: QueryEngine exposes a test-only stream override

`cc_query::engine::QueryEngine` MUST accept a scripted stream
producer in test builds so `run_turn`-level regression tests can
drive the full turn loop without a live API.

The override SHALL be a `#[cfg(test)]`-gated field and a
`#[cfg(test)]`-gated builder method. Production (non-test) builds
SHALL NOT contain the field or the branch that reads it — the
library bytes emitted by `cargo build --release -p cc-query` before
and after this change must be identical modulo build metadata.

When the override is `None`, `run_turn` SHALL invoke
`self.api.stream_message(...)` exactly as it did pre-change. When
`Some(fn)`, `run_turn` SHALL invoke `fn(&req, cancel)` and consume
the returned `mpsc::Receiver` as if it came from the live client.

#### Scenario: Override replaces the live stream

- **Given** a `QueryEngine` built with
  `with_stream_override(scripted_stream(vec![Ok(MessageStart),
  Ok(text-delta "hi"), Ok(MessageStop)]))`
- **When** `engine.run_turn("hello", ignore, &mut messages,
  &cancel).await` runs
- **Then** the returned final text is `"hi"`
- **And** no network call was attempted (the real
  `ApiClient::stream_message` was not entered)

#### Scenario: Cancel short-circuits the scripted stream

- **Given** the same engine
- **And** the token is cancelled before `run_turn` is called
- **When** `run_turn` runs
- **Then** it returns `Err(CcError::Cancelled)` within the usual
  cancel-latency budget
- **And** the engine does not hang waiting for more scripted events

#### Scenario: Release build has no override field

- **Given** the `cc-query` crate built with `cargo build
  --release`
- **When** the library's symbol table is inspected
- **Then** no `stream_override` symbol is present
- **And** the generated machine code for `run_turn`'s stream
  acquisition matches pre-change byte-for-byte modulo build
  metadata

### Requirement: scripted_stream helper covers the common case

The `test_support::scripted_stream(events: Vec<CcResult<StreamEvent>>)` helper MUST return a closure compatible with `with_stream_override`. Each invocation of the closure SHALL yield a fresh `mpsc::Receiver` pre-loaded with the event sequence. Events SHALL be delivered in input order.

If `CcError` is not `Clone`, scripted errors MAY be degraded to
`CcError::Other("scripted-stream error")`; tests needing
error-variant fidelity SHALL use `with_stream_override` directly
with a stateful closure that constructs errors per-call.

#### Scenario: Multi-call closure produces independent receivers

- **Given** a `scripted_stream(vec![Ok(MessageStart)])` closure
- **When** it is invoked twice within the same test
- **Then** each call yields a receiver whose first (and only)
  message is `Ok(MessageStart)`
- **And** neither receiver affects the other's event stream
