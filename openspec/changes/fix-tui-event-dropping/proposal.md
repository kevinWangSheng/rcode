## Why

`cc-query/src/engine.rs:182, 187, 192` emits streaming updates to the TUI
with `try_send`, then silently drops them on failure:

```rust
let _ = tx.try_send(AppEvent::StreamDelta(text.clone()));
let _ = tx.try_send(AppEvent::StreamThinking(thinking.clone()));
let _ = tx.try_send(AppEvent::StreamToolUse(...));
```

The TUI event channel is bounded. Under any backpressure — slow render,
terminal blocked, user holding a key, large paste in the input box —
`try_send` fails and the engine loses the token forever. The user sees:

- streamed assistant text with missing chunks in the middle,
- thinking blocks that never render,
- tool-use widgets that never appear even though the tool actually ran.

This violates the M3 exit criterion "streaming token-by-token; no drops
under normal operation" and breaks the AC-4 memory-flatness assumption
(the engine thinks the TUI saw everything; it didn't). The loss is silent
because `let _ =` discards the `TrySendError`.

## What Changes

- Replace `try_send` with `send().await` on the hot path. The engine is
  already inside an async block (`complete_message` is `.await`ed), so a
  backpressuring send is the correct primitive: it slows the engine
  naturally when the TUI is slow, rather than discarding user-visible data.
- Keep the channel bounded (a small capacity — e.g., 256 — is fine) so
  memory stays flat. Backpressure, not dropping, is the control mechanism.
- Distinguish "channel closed" (TUI gone) from "channel full": the former
  is a cancellation signal and should propagate back to the engine to
  short-circuit the turn; the latter now just awaits capacity.
- Add a debug-build-only counter / metric so regressions ("I added a new
  try_send somewhere") surface in tests.
- Unit test: simulate a slow consumer and verify every StreamDelta is
  delivered in order, none dropped.

## Capabilities

### Modified Capabilities
- `query-engine-events`: the engine MUST deliver every `StreamDelta`,
  `StreamThinking`, and `StreamToolUse` to the TUI channel in order, and
  MUST NOT silently drop events under backpressure.

## Impact

- **Affected code:** `cc-query/src/engine.rs` (the three `try_send` call
  sites plus any sibling emission points), `cc-tui/src/lib.rs` (event
  loop may want to tune its `tick` / render cadence once backpressure is
  in play so it doesn't starve keyboard input).
- **Behavior:** a visibly slow TUI now throttles the engine instead of
  racing ahead and losing data. Perceived latency goes up slightly in
  pathological cases; correctness goes up a lot.
- **Risk:** MEDIUM. The engine is now coupled to TUI liveness. If the TUI
  event loop deadlocks, the engine stalls too. Mitigation: the TUI
  `events_rx.recv()` branch already races against input + tick; a truly
  stuck TUI will be killed by the user's Ctrl+C which cancels the
  engine's CancellationToken before the send can block forever.
