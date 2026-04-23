# Proposal — Scriptable stream-producer override for engine tests

## Why

`QueryEngine::run_turn` hands off to the live API at
`engine.rs:263`:

```rust
let rx = self.api.stream_message(req, cancel).await?;
```

Every turn-level regression test today either (a) drives
`drain_stream` directly (bypassing the turn loop entirely, so tests
can't see cancel-path cleanup, `pending_additional_contexts`
clearing, or cache-breakpoint tagging over multiple iterations), or
(b) is deferred with a note like:

> 5.3 `additional_contexts_are_cleared_on_cancel` — Deferred:
> triggering the cancel branch needs a mock
> `ApiClient::stream_message`; the engine's test surface only
> drives `drain_stream` directly.
> — `fix-hook-correctness-wiring/tasks.md:107-115`

Both cancel-path regression tests in
`fix-file-history-snapshot-producers` have the same blocker.
Rather than each follow-up change growing its own ad-hoc harness
(three nearly-identical stubs, none reusable), ship one shared
harness here.

## Goal

Add a scriptable, `#[cfg(test)]`-gated override on `QueryEngine` so
tests can supply a canned `Vec<CcResult<StreamEvent>>` (or a closure
that yields them) in place of the live stream. The override is
invisible to production code; if unset, behaviour is identical to
today.

Not a trait extraction of `ApiClient`. A full `ApiTransport` trait
is a bigger architectural change (changes `QueryEngine::api` from
concrete to `Arc<dyn ApiTransport>`; ripples into `QueryEngineConfig`)
and belongs to a separate refactor if it's ever wanted. The test
hook is strictly additive and stays out of the production hot path.

## What changes

### 1. Add the override field

```rust
// rust/crates/cc-query/src/engine.rs inside struct QueryEngine
#[cfg(test)]
stream_override: Option<StreamOverrideFn>,
```

where

```rust
// in a new engine::test_support module (same file, #[cfg(test)] gated)
pub(crate) type StreamOverrideFn = std::sync::Arc<
    dyn Fn(&CreateMessageRequest, &CancellationToken)
            -> tokio::sync::mpsc::Receiver<CcResult<StreamEvent>>
        + Send
        + Sync,
>;
```

Default `None`. Initialised to `None` in `QueryEngine::new`.

### 2. Route the override through `run_turn`

Replace the single `stream_message` call at `engine.rs:263`:

```rust
#[cfg(test)]
let rx = if let Some(override_fn) = &self.stream_override {
    override_fn(&req, cancel)
} else {
    self.api.stream_message(req, cancel).await?
};
#[cfg(not(test))]
let rx = self.api.stream_message(req, cancel).await?;
```

The `#[cfg]` gates keep production cold-path cost at zero —
release builds compile to the same code they did before this
change.

### 3. Public test-support API

Expose a builder-style setter in a `test_support` submodule so test
files don't have to poke at the private field:

```rust
// rust/crates/cc-query/src/engine.rs (bottom, #[cfg(test)] gated)
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use tokio::sync::mpsc;

    impl QueryEngine {
        /// Replace the live API stream with a scripted producer.
        /// Only available in test builds.
        pub(crate) fn with_stream_override<F>(mut self, f: F) -> Self
        where
            F: Fn(&CreateMessageRequest, &CancellationToken)
                -> mpsc::Receiver<CcResult<StreamEvent>>
                + Send
                + Sync
                + 'static,
        {
            self.stream_override = Some(std::sync::Arc::new(f));
            self
        }
    }

    /// Ship a canned Vec of events. Cheap helper for the common
    /// "here are the 4 StreamEvents this turn should see" case.
    ///
    /// Each call to the returned closure produces a fresh receiver
    /// pre-loaded with the same script. Tests with multi-turn flows
    /// that want different scripts per iteration should use
    /// `with_stream_override` directly with stateful logic.
    pub(crate) fn scripted_stream(
        events: Vec<CcResult<StreamEvent>>,
    ) -> impl Fn(&CreateMessageRequest, &CancellationToken)
        -> mpsc::Receiver<CcResult<StreamEvent>>
        + Send
        + Sync
        + 'static {
        let events = std::sync::Arc::new(events);
        move |_req, _cancel| {
            let (tx, rx) = mpsc::channel(events.len().max(1));
            let events = events.clone();
            tokio::spawn(async move {
                for ev in events.iter() {
                    // Clone because CcError isn't Clone; wrap in Arc
                    // or rebuild the event. See §4.
                    let cloned = match ev {
                        Ok(e) => Ok(e.clone()),
                        Err(_) => Err(cc_core::CcError::Other(
                            "scripted-stream error".into(),
                        )),
                    };
                    if tx.send(cloned).await.is_err() {
                        break;
                    }
                }
            });
            rx
        }
    }
}
```

The `scripted_stream` helper covers the 80% case where a test just
wants to replay a short, fixed sequence. More complex scenarios
(multi-turn scripts, cancel mid-event) use `with_stream_override`
with a stateful closure.

### 4. Error-value workaround

`CcError` is not `Clone`. The helper above accepts that and degrades
any scripted error to `CcError::Other("scripted-stream error")`.
Tests that need a specific error variant can pass
`with_stream_override` directly and construct the error per-call.

If a future test needs error fidelity, the cheapest fix is to add
`#[derive(Clone)]` to `CcError`. Leave that as a follow-up unless
it actually blocks someone.

### 5. Usage example (for the test writer)

```rust
// in cc-query/src/engine.rs::tests
#[tokio::test]
async fn example_cancel_after_text_delta() {
    use super::test_support::*;
    let engine = build_test_engine(/* ... */)
        .with_stream_override(scripted_stream(vec![
            Ok(StreamEvent::MessageStart { /* ... */ }),
            Ok(StreamEvent::ContentBlockStart { /* text block */ }),
            Ok(StreamEvent::ContentBlockDelta { /* "hello" */ }),
        ]));
    let cancel = CancellationToken::new();
    cancel.cancel(); // cancel immediately
    let mut messages = vec![];
    let result = engine.run_turn("hi", |_| {}, &mut messages, &cancel).await;
    assert!(matches!(result, Err(cc_core::CcError::Cancelled)));
    // Now the engine's cancel path ran — test can assert on session
    // JSONL, on pending_additional_contexts being cleared, etc.
}
```

## Impact

- **Affected specs**: new `engine-stream-mock-harness` capability
  (this change).
- **Affected crates**: `cc-query` only. Zero public-API change;
  the `#[cfg(test)]` gates ensure release builds are byte-identical
  to pre-change.
- **Downstream unblocks**:
  - `fix-hook-correctness-wiring` §5.3 (the 1 currently deferred
    task).
  - `fix-file-history-snapshot-producers` §6.1 / §6.2 (cancel-path
    markers tests).
  - Any future engine-level regression test that needs to drive
    a full turn without a live API.
- **Behaviour change**: none in production. The override field is
  absent in non-test builds.

## Open questions

1. Should the harness live under a `test-support` cargo feature
   instead of `#[cfg(test)]`, so integration tests in other crates
   can reach it? The audits only call out in-crate engine tests, so
   `#[cfg(test)]` is sufficient for now. Promote to a feature if a
   cross-crate use case materialises.

2. `scripted_stream` spawns a tokio task per call. Acceptable for
   test timing; if tests become flaky on this, switch to a
   synchronous pre-fill: create the channel, send all events
   before returning the rx. (The current `mpsc::channel` capacity
   covers the typical test event count, so there's no backpressure
   risk.)

3. If `#[derive(Clone)]` on `CcError` is easy (no non-Clone fields
   leaked), fold it into this change to get error-fidelity for
   free. Check the enum definition first.
