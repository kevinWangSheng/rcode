//! Integration test: the query-engine event forwarder must not drop
//! `StreamDelta` / `StreamThinking` / `StreamToolUse` events under TUI
//! backpressure.
//!
//! Regression guard for the fix-tui-event-dropping proposal (fix C3) —
//! prior behaviour used `tx.try_send(...)` + `let _ =` which silently
//! dropped events when the bounded channel was full.

use cc_api::{ContentBlockDelta, ContentBlockStartData, StreamEvent};
use cc_core::CcResult;
use cc_query::events::{forward_stream_events, ForwardOutcome};
use cc_query::AppEvent;
use tokio::sync::mpsc;

fn text_delta(s: &str) -> StreamEvent {
    StreamEvent::ContentBlockDelta {
        index: 0,
        delta: ContentBlockDelta::TextDelta { text: s.into() },
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_thousand_deltas_slow_consumer_no_drops() {
    // Bounded channel as used in practice — keeping a small capacity
    // exercises backpressure repeatedly during the test.
    let (in_tx, mut in_rx) = mpsc::channel::<CcResult<StreamEvent>>(16);
    let (out_tx, mut out_rx) = mpsc::channel::<AppEvent>(16);

    // Fast producer pushing 1000 text deltas.
    let producer = tokio::spawn(async move {
        for i in 0..1000 {
            in_tx
                .send(Ok(text_delta(&format!("d{i}"))))
                .await
                .expect("producer send");
        }
        drop(in_tx);
    });

    // Forwarder uses `send().await` → no drops under backpressure.
    let forwarder =
        tokio::spawn(async move { forward_stream_events(&mut in_rx, &out_tx).await });

    // Slow consumer: 1 ms between reads.
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
    let outcome = forwarder.await.unwrap().expect("forward result");
    assert_eq!(outcome, ForwardOutcome::Completed);

    assert_eq!(received.len(), 1000, "every delta was delivered");
    for (i, s) in received.iter().enumerate() {
        assert_eq!(s, &format!("d{i}"), "delivered in order at index {i}");
    }
}

#[tokio::test]
async fn closing_consumer_aborts_forwarder_cleanly() {
    let (in_tx, mut in_rx) = mpsc::channel::<CcResult<StreamEvent>>(4);
    let (out_tx, out_rx) = mpsc::channel::<AppEvent>(1);

    drop(out_rx); // TUI disappeared

    let _producer = tokio::spawn(async move {
        for i in 0..100 {
            let _ = in_tx.send(Ok(text_delta(&format!("d{i}")))).await;
        }
    });

    let outcome = forward_stream_events(&mut in_rx, &out_tx)
        .await
        .expect("no transport error");
    assert_eq!(outcome, ForwardOutcome::Cancelled);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mixed_event_stream_is_delivered_in_order() {
    let (in_tx, mut in_rx) = mpsc::channel::<CcResult<StreamEvent>>(4);
    let (out_tx, mut out_rx) = mpsc::channel::<AppEvent>(4);

    tokio::spawn(async move {
        in_tx.send(Ok(text_delta("a"))).await.unwrap();
        in_tx
            .send(Ok(StreamEvent::ContentBlockStart {
                index: 1,
                content_block: ContentBlockStartData::ToolUse {
                    id: "toolu_1".into(),
                    name: "read".into(),
                    input: serde_json::json!({}),
                },
            }))
            .await
            .unwrap();
        in_tx
            .send(Ok(StreamEvent::ContentBlockDelta {
                index: 2,
                delta: ContentBlockDelta::ThinkingDelta {
                    thinking: "t".into(),
                },
            }))
            .await
            .unwrap();
        in_tx.send(Ok(text_delta("b"))).await.unwrap();
        drop(in_tx);
    });

    let handle =
        tokio::spawn(async move { forward_stream_events(&mut in_rx, &out_tx).await });

    let mut evs = Vec::new();
    while let Some(ev) = out_rx.recv().await {
        // Slow the consumer a bit so we really do hit backpressure on a
        // cap-4 channel.
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        evs.push(ev);
    }
    assert_eq!(handle.await.unwrap().unwrap(), ForwardOutcome::Completed);

    assert_eq!(evs.len(), 4);
    assert!(matches!(evs[0], AppEvent::StreamDelta(ref s) if s == "a"));
    assert!(matches!(evs[1], AppEvent::StreamToolUse(ref tu) if tu.id == "toolu_1"));
    assert!(matches!(evs[2], AppEvent::StreamThinking(ref s) if s == "t"));
    assert!(matches!(evs[3], AppEvent::StreamDelta(ref s) if s == "b"));
}
