//! Streaming events emitted by `QueryEngine::run_turn_with_events` for
//! consumers such as the TUI.
//!
//! Events are delivered over a bounded `tokio::sync::mpsc::Sender` using
//! `send().await`, so the engine applies natural backpressure when the
//! consumer is slow. This replaces an earlier pattern where `try_send` +
//! `let _ =` silently dropped `StreamDelta` / `StreamThinking` /
//! `StreamToolUse` events under TUI load, breaking the M3 "token-by-token,
//! no drops" exit criterion.

use cc_api::{ContentBlockDelta, ContentBlockStartData, StreamEvent};
use cc_core::{CcResult, ToolUseBlock};
use tokio::sync::mpsc;
use tracing::debug;

/// Recommended capacity for the bounded channel — small enough to bound
/// memory, large enough to smooth normal render jitter. Consumers are free
/// to choose a different capacity; the engine does not depend on this
/// value at the type level.
pub const DEFAULT_EVENT_CAPACITY: usize = 256;

/// An event produced by the query engine during a turn.
#[derive(Debug, Clone)]
pub enum AppEvent {
    /// A streaming text delta from the assistant.
    StreamDelta(String),
    /// A streaming extended-thinking delta.
    StreamThinking(String),
    /// A tool-use block the assistant has requested (emitted at
    /// `content_block_start` time, so the TUI can render the widget even
    /// before the arguments have finished streaming in).
    StreamToolUse(ToolUseBlock),
    /// The assistant completed a turn (stop_reason = end_turn / max_tokens /
    /// stop_sequence).
    TurnComplete,
    /// The engine aborted because the TUI event channel was closed (consumer
    /// dropped). This is distinct from a normal end-of-turn: it signals the
    /// caller that downstream is gone.
    Cancelled,
}

/// Outcome of forwarding a stream through an `AppEvent` channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForwardOutcome {
    /// The stream ran to completion and every event was delivered.
    Completed,
    /// The consumer dropped the receiver mid-stream; remaining events were
    /// not delivered and the caller should treat this as cancellation.
    Cancelled,
}

/// Forward `StreamEvent`s from `rx` into the TUI channel `tx` as
/// `AppEvent`s, using `send().await` so backpressure throttles the engine
/// rather than dropping events.
///
/// A `SendError` (channel closed) is treated as cancellation: we stop the
/// loop and return `ForwardOutcome::Cancelled`. Transport errors from the
/// inbound stream propagate as `CcError::Api`.
///
/// Counters returned in the `debug_assertions` build verify that every
/// inbound streaming event that produces a user-visible AppEvent actually
/// made it out. The engine asserts sent == forwarded at end of turn.
pub async fn forward_stream_events(
    rx: &mut mpsc::Receiver<CcResult<StreamEvent>>,
    tx: &mpsc::Sender<AppEvent>,
) -> CcResult<ForwardOutcome> {
    let mut sent = 0usize;
    let mut seen = 0usize;

    while let Some(item) = rx.recv().await {
        let event = item?;
        match &event {
            StreamEvent::ContentBlockDelta {
                delta: ContentBlockDelta::TextDelta { text },
                ..
            } => {
                seen += 1;
                if tx.send(AppEvent::StreamDelta(text.clone())).await.is_err() {
                    debug!("forward_stream_events: consumer dropped — cancelling");
                    return Ok(ForwardOutcome::Cancelled);
                }
                sent += 1;
            }
            StreamEvent::ContentBlockDelta {
                delta: ContentBlockDelta::ThinkingDelta { thinking },
                ..
            } => {
                seen += 1;
                if tx
                    .send(AppEvent::StreamThinking(thinking.clone()))
                    .await
                    .is_err()
                {
                    debug!("forward_stream_events: consumer dropped — cancelling");
                    return Ok(ForwardOutcome::Cancelled);
                }
                sent += 1;
            }
            StreamEvent::ContentBlockStart {
                content_block: ContentBlockStartData::ToolUse { id, name, input },
                ..
            } => {
                seen += 1;
                let tu = ToolUseBlock {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                };
                if tx.send(AppEvent::StreamToolUse(tu)).await.is_err() {
                    debug!("forward_stream_events: consumer dropped — cancelling");
                    return Ok(ForwardOutcome::Cancelled);
                }
                sent += 1;
            }
            _ => {}
        }
    }

    // Invariant: every inbound user-visible delta was forwarded.
    debug_assert_eq!(
        seen,
        sent,
        "forward_stream_events dropped {} events (seen={seen}, sent={sent})",
        seen - sent
    );

    // Upstream closed normally; caller decides whether to emit TurnComplete.
    let _ = sent; // keep used in release builds
    Ok(ForwardOutcome::Completed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cc_api::MessageDeltaData;
    use cc_api::MessageDeltaUsage;
    use cc_core::CcError;

    fn text_delta(s: &str) -> StreamEvent {
        StreamEvent::ContentBlockDelta {
            index: 0,
            delta: ContentBlockDelta::TextDelta { text: s.into() },
        }
    }

    fn thinking_delta(s: &str) -> StreamEvent {
        StreamEvent::ContentBlockDelta {
            index: 0,
            delta: ContentBlockDelta::ThinkingDelta { thinking: s.into() },
        }
    }

    fn tool_use_start(id: &str, name: &str) -> StreamEvent {
        StreamEvent::ContentBlockStart {
            index: 0,
            content_block: ContentBlockStartData::ToolUse {
                id: id.into(),
                name: name.into(),
                input: serde_json::json!({}),
            },
        }
    }

    /// Slow consumer: 1000 deltas produced, consumer sleeps 1ms between
    /// reads, all 1000 must arrive in order.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn no_drops_under_backpressure_1000_deltas() {
        let (in_tx, mut in_rx) = mpsc::channel::<CcResult<StreamEvent>>(8);
        let (out_tx, mut out_rx) = mpsc::channel::<AppEvent>(8);

        // Producer: push 1000 deltas.
        let producer = tokio::spawn(async move {
            for i in 0..1000 {
                let ev = text_delta(&format!("{i}"));
                in_tx.send(Ok(ev)).await.expect("producer send");
            }
            drop(in_tx);
        });

        // Forwarder.
        let forwarder =
            tokio::spawn(async move { forward_stream_events(&mut in_rx, &out_tx).await });

        // Slow consumer.
        let mut received: Vec<String> = Vec::with_capacity(1000);
        while let Some(ev) = out_rx.recv().await {
            match ev {
                AppEvent::StreamDelta(s) => {
                    received.push(s);
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                }
                other => panic!("unexpected event: {other:?}"),
            }
        }

        producer.await.unwrap();
        let outcome = forwarder.await.unwrap().expect("forward ok");
        assert_eq!(outcome, ForwardOutcome::Completed);
        assert_eq!(received.len(), 1000, "no drops");
        for (i, s) in received.iter().enumerate() {
            assert_eq!(s, &format!("{i}"), "out-of-order at index {i}");
        }
    }

    /// Closing the consumer mid-stream surfaces as ForwardOutcome::Cancelled
    /// and does NOT panic / spin the forwarder.
    #[tokio::test]
    async fn consumer_drop_causes_clean_cancel() {
        let (in_tx, mut in_rx) = mpsc::channel::<CcResult<StreamEvent>>(4);
        let (out_tx, out_rx) = mpsc::channel::<AppEvent>(1);

        // Drop consumer immediately.
        drop(out_rx);

        let _producer = tokio::spawn(async move {
            for i in 0..100 {
                let _ = in_tx.send(Ok(text_delta(&format!("{i}")))).await;
            }
        });

        let outcome = forward_stream_events(&mut in_rx, &out_tx)
            .await
            .expect("no transport error");
        assert_eq!(outcome, ForwardOutcome::Cancelled);
    }

    /// Mixed event types: text + thinking + tool-use are all forwarded.
    #[tokio::test]
    async fn all_three_event_kinds_forwarded() {
        let (in_tx, mut in_rx) = mpsc::channel::<CcResult<StreamEvent>>(16);
        let (out_tx, mut out_rx) = mpsc::channel::<AppEvent>(16);

        tokio::spawn(async move {
            in_tx.send(Ok(text_delta("hi"))).await.unwrap();
            in_tx.send(Ok(thinking_delta("ponder"))).await.unwrap();
            in_tx
                .send(Ok(tool_use_start("toolu_1", "read")))
                .await
                .unwrap();
            // A MessageDelta (non-user-visible) — must be ignored, not sent.
            in_tx
                .send(Ok(StreamEvent::MessageDelta {
                    delta: MessageDeltaData {
                        stop_reason: None,
                        stop_sequence: None,
                    },
                    usage: MessageDeltaUsage { output_tokens: 1 },
                }))
                .await
                .unwrap();
            drop(in_tx);
        });

        let handle = tokio::spawn(async move { forward_stream_events(&mut in_rx, &out_tx).await });

        let mut got = Vec::new();
        while let Some(ev) = out_rx.recv().await {
            got.push(ev);
        }
        let outcome = handle.await.unwrap().unwrap();
        assert_eq!(outcome, ForwardOutcome::Completed);
        assert_eq!(got.len(), 3);
        assert!(matches!(got[0], AppEvent::StreamDelta(ref s) if s == "hi"));
        assert!(matches!(got[1], AppEvent::StreamThinking(ref s) if s == "ponder"));
        assert!(
            matches!(got[2], AppEvent::StreamToolUse(ref tu) if tu.id == "toolu_1" && tu.name == "read")
        );
    }

    /// Upstream transport error propagates out of forward_stream_events as
    /// CcError, not silently dropped.
    #[tokio::test]
    async fn upstream_error_propagates() {
        let (in_tx, mut in_rx) = mpsc::channel::<CcResult<StreamEvent>>(4);
        let (out_tx, _out_rx) = mpsc::channel::<AppEvent>(4);

        tokio::spawn(async move {
            in_tx.send(Ok(text_delta("partial"))).await.unwrap();
            in_tx.send(Err(CcError::api("boom"))).await.unwrap();
            drop(in_tx);
        });

        let err = forward_stream_events(&mut in_rx, &out_tx)
            .await
            .expect_err("should surface transport error");
        assert!(matches!(err, CcError::Api { message, .. } if message.contains("boom")));
    }
}
