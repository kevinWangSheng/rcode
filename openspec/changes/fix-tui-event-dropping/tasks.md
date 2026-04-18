## 1. Engine Emission

- [ ] 1.1 In `cc-query/src/engine.rs`, replace each `tx.try_send(...)` on
      the streaming path with `tx.send(...).await` guarded by the
      engine's cancellation token (`tokio::select!` against `cancel.cancelled()`).
- [ ] 1.2 On `SendError` (channel closed), treat it as cancellation:
      break out of the stream loop and return a cancelled `TurnOutcome`.
- [ ] 1.3 Keep (or introduce) a small bounded capacity (256) to bound
      memory.

## 2. Channel Plumbing

- [ ] 2.1 Confirm the channel type in `cc-tui` is `mpsc::Sender<AppEvent>`
      with a sensible capacity. Tune if needed.
- [ ] 2.2 Remove any duplicate `try_send` sites discovered by
      `rg "try_send" cc-query/`.

## 3. Test: No Drops Under Backpressure

- [ ] 3.1 New test in `cc-query/tests/backpressure.rs`: create a
      bounded channel, drive a mock stream that emits 1000 deltas, have
      the consumer sleep 1ms between reads, assert all 1000 received in
      order.
- [ ] 3.2 Test that closing the consumer causes the engine to abort
      cleanly (not panic, not spin).

## 4. Observability

- [ ] 4.1 `debug_assert!` or counter in the engine that tracks "events
      sent" and compares to "events actually emitted" at turn end; fail
      the test if they diverge.

## 5. Sign-off

- [ ] 5.1 `cargo test --workspace` + `cargo clippy --workspace -- -D warnings`
      clean.
- [ ] 5.2 Manual: run a long-running turn (force a multi-thousand-token
      response) and confirm the TUI transcript matches the final message
      exactly.
