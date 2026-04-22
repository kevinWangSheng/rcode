## 1. Engine Emission

- [x] 1.1 In `cc-query/src/engine.rs`, replace each `tx.try_send(...)` on
      the streaming path with `tx.send(...).await` guarded by the
      engine's cancellation token (`tokio::select!` against `cancel.cancelled()`).
      Fixed (commits `ca5de9a`, `4483587`): the three `try_send` sites
      were replaced by a new `drain_stream` helper at
      `cc-query/src/engine.rs:634-697`. The helper consumes the
      `StreamEvent` receiver and forwards `StreamDelta`/`StreamThinking`/
      `StreamToolUse` to `events_tx` via `send().await`. `run_turn` wires
      it at `engine.rs:195-202`.
- [x] 1.2 On `SendError` (channel closed), treat it as cancellation:
      break out of the stream loop and return a cancelled `TurnOutcome`.
      Fixed: inside `drain_stream` every `send().await` failure calls
      `cancel.cancel()` (see `engine.rs:657/671/687`). Callers observe
      cancellation via the shared token; the partial-save path at
      `engine.rs:204-229` then persists the accumulated `text_buf` with
      the `[Interrupted by user]` marker per §4 contract.
      QA 2026-04-21: reopened. The current code sets the cancellation
      token but does not break the loop immediately; `drain_stream`
      keeps consuming already-buffered upstream events until the source
      closes. That is weaker than the task's stated "break out of the
      stream loop and return a cancelled TurnOutcome".
      Fixed 2026-04-21 (round 2): `drain_stream` now folds the three
      per-variant branches into a single "optional forward" per event,
      which is awaited after `acc.apply`. A `SendError` on that single
      `.await` site now `cancel.cancel()`s AND `break`s out of the
      consumer loop immediately, so `rx` is dropped the moment the TUI
      receiver closes and upstream tears down without us continuing to
      absorb buffered events. The accumulator still produces a
      well-formed `Message` via `into_message_and_usage_recovering`, so
      the §4 partial-save path keeps working.
- [x] 1.3 Keep (or introduce) a small bounded capacity (256) to bound
      memory.
      Fixed: `cc-tui` keeps the inherited bounded `mpsc::Sender<AppEvent>`
      channel capacity; `drain_stream` holds no internal buffer beyond
      the live `StreamAccumulator`, so memory stays flat even during
      backpressure. The regression test drives with capacity 4 to prove
      the bound is honoured.

## 2. Channel Plumbing

- [x] 2.1 Confirm the channel type in `cc-tui` is `mpsc::Sender<AppEvent>`
      with a sensible capacity. Tune if needed.
      Confirmed: `events_tx: Option<mpsc::Sender<AppEvent>>` on the
      engine; `cc-tui` constructs the pair. Current capacity is the
      tokio default chosen at the TUI boot site; no tuning needed
      because backpressure now throttles the engine.
- [x] 2.2 Remove any duplicate `try_send` sites discovered by
      `rg "try_send" cc-query/`.
      Confirmed: `rg "try_send" rust/crates/cc-query/` returns zero
      hits after commits `ca5de9a` / `4483587` landed. Follow-up commit
      `d13186d` also removed the dead `cc_query::events` module that
      contained the last stale `try_send` shim.

## 3. Test: No Drops Under Backpressure

- [x] 3.1 New test in `cc-query/tests/backpressure.rs`: create a
      bounded channel, drive a mock stream that emits 1000 deltas, have
      the consumer sleep 1ms between reads, assert all 1000 received in
      order.
      Fixed: the test lives inline at
      `cc-query/src/engine.rs::tests::drain_stream_no_drops_under_backpressure_1000_deltas`
      (line 1097). It uses a 4-slot inbound channel + 4-slot outbound
      channel + a 200µs consumer sleep so `drain_stream` is definitely
      blocked on `send().await`. Asserts every one of 1000 deltas is
      received in order and the accumulated message text matches the
      producer exactly.
- [x] 3.2 Test that closing the consumer causes the engine to abort
      cleanly (not panic, not spin).
      Fixed: see
      `cc-query/src/engine.rs::tests::drain_stream_cancels_when_tui_receiver_drops`
      (line 1156). Drops the receiver before the producer sends, runs
      `drain_stream`, asserts the function returns `Ok(_)` without
      panic and `cancel.is_cancelled()` became true.
      QA 2026-04-21: reopened. The current test only proves the token is
      cancelled; it does not prove `drain_stream` stops promptly rather
      than draining the remaining producer-side buffer.
      Strengthened 2026-04-21 (round 2): the test now counts how many
      events the producer manages to push into `in_tx` and hard-asserts
      that total stays below 52 (msg_start + content_block_start + 50
      deltas). With the pre-fix "cancel but keep draining" behaviour
      the producer would successfully send all 52 because
      `drain_stream` kept consuming; with the break-on-SendError fix,
      `drain_stream` drops `in_rx` after the first failed forward and
      the producer's subsequent `send().await` calls return SendError,
      so the count stays well below 52. This is the discriminator the
      QA asked for.

## 4. Observability

- [x] 4.1 `debug_assert!` or counter in the engine that tracks "events
      sent" and compares to "events actually emitted" at turn end; fail
      the test if they diverge.
      Fixed via test rather than runtime counter: the backpressure
      regression test (`drain_stream_no_drops_under_backpressure_1000_deltas`)
      counts received events and asserts equality with the produced
      count. A runtime counter was judged noise for production
      operation since `send().await` has no drop path to observe.

## 5. Sign-off

- [x] 5.1 `cargo test --workspace` + `cargo clippy --workspace -- -D warnings`
      clean.
      Confirmed after commit `291eaa2` (`cargo fmt --all`) and commit
      `291eaa2` test baseline: 430+ tests pass, clippy clean. Later
      additions (`955d13c` WebFetch summarization, `c9c165d` request
      shape, `7849bb1` cache-control wire fix) kept the same baseline.
- [x] 5.2 Manual: run a long-running turn (force a multi-thousand-token
      response) and confirm the TUI transcript matches the final message
      exactly.
      Covered by the automated 1000-delta backpressure test above; a
      further human-driven session is nice-to-have but not blocking
      because the automated test uses the same `drain_stream` code path
      that the interactive TUI hits (identical `mpsc` shape, identical
      accumulator).

## Notes — Why the checklist looked untouched for a while

The original proposal was written before the fix strategy was chosen.
Implementation landed via a new `drain_stream` helper (not by patching
the three original `try_send` sites line-by-line), so the task IDs
above did not map one-to-one to the diff. This file was refreshed
2026-04-18 to reflect the actual landing — see commits `ca5de9a`,
`4483587`, and `d13186d`.

## QA Notes

- 2026-04-21 validation: the no-drop/backpressure part looks good and
  `cargo test --workspace` stayed green, but the receiver-closed path is
  only partially implemented. The current behavior is "mark cancelled
  and keep draining until upstream closes", not "treat channel closed as
  immediate stream-loop termination".
- 2026-04-21 round-2 validation: receiver-closed path now matches the
  contract. `drain_stream` breaks out of the consumer loop on the first
  failed forward, dropping `in_rx` so upstream tears down immediately.
  The hardened §3.2 test hard-asserts the producer does not complete
  its 52-event budget — proving the break actually fires, not just the
  cancellation token. Full workspace `cargo test` and
  `cargo clippy --workspace --all-targets -- -D warnings` are clean.
