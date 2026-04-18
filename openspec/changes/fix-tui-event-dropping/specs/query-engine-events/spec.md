## ADDED Requirements

### Requirement: Lossless Stream Event Delivery

The query engine SHALL deliver every `StreamDelta`, `StreamThinking`, and
`StreamToolUse` event it emits to the TUI event channel, in the order
emitted, with no silent drops.

The engine MAY apply backpressure (i.e., the streaming read from the
Anthropic API is paused while the TUI catches up). The engine MUST NOT
use `try_send` / non-awaiting sends on these event types.

If the TUI channel is closed (receiver dropped), the engine SHALL treat
it as cancellation of the current turn and terminate the stream loop
cleanly, without panic and without a busy spin.

#### Scenario: Slow consumer receives everything
- **GIVEN** a consumer that sleeps 1ms between each receive
- **AND** a streamed response that emits 1000 `StreamDelta` events
- **WHEN** the engine finishes the turn
- **THEN** the consumer has received all 1000 events in the original order
- **AND** no `TrySendError` / drop counter is incremented

#### Scenario: Closed channel aborts the turn
- **GIVEN** an in-progress turn with events actively streaming
- **WHEN** the consumer drops the receiver
- **THEN** the engine's next `.send(...).await` returns `SendError`
- **AND** the engine aborts the turn with a cancelled outcome
- **AND** no panic is raised and no event-loop spin occurs

#### Scenario: Backpressure does not lose tokens
- **WHEN** the TUI render loop is busy for longer than the time between
  two stream events
- **THEN** the engine waits (does not drop) before emitting the next event
- **AND** the final assistant message in the transcript contains every
  token that was streamed
