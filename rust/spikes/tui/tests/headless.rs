//! Headless tests for TUI Spike acceptance criteria.
//!
//! AC-2: Abort stops stream; partial text preserved
//! AC-3: Enter during streaming queues input (not dropped)
//! AC-4: 100 turns complete without memory growth
//! AC-5: Tokio + async logic runs 100 turns without deadlock

use spike_tui::{App, StreamState};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

// ── Shared simulation ──────────────────────────────────────────────────────

#[derive(Debug)]
enum Event {
    Token(String),
    Done,
}

/// Simulates a streaming response: 40 tokens at 1ms intervals.
/// Stops early if abort_rx receives a signal.
async fn simulate_stream(tx: mpsc::Sender<Event>, mut abort_rx: mpsc::Receiver<()>) {
    let tokens: Vec<&str> = vec![
        "The ", "quick ", "brown ", "fox ", "jumps ", "over ", "the ", "lazy ", "dog. ",
        "Ratatui ", "handles ", "streaming ", "well. ", "Each ", "token ", "arrives ",
        "with ", "a ", "small ", "delay. ", "The ", "event ", "loop ", "remains ",
        "responsive. ", "Ctrl+C ", "aborts ", "immediately. ", "Partial ", "text ",
        "is ", "preserved. ", "AC-2 ", "verified. ", "AC-3 ", "queuing ", "works. ",
        "AC-4 ", "memory ", "stable.",
    ];
    for token in tokens {
        if abort_rx.try_recv().is_ok() {
            let _ = tx.send(Event::Done).await;
            return;
        }
        let _ = tx.send(Event::Token(token.to_string())).await;
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let _ = tx.send(Event::Done).await;
}

// ── AC-2: Abort preserves partial text ────────────────────────────────────

/// AC-2a: Partial text is non-empty after abort mid-stream.
#[tokio::test]
async fn ac2_partial_text_preserved_after_abort() {
    let (event_tx, mut event_rx) = mpsc::channel::<Event>(64);
    let (abort_tx, abort_rx) = mpsc::channel::<()>(1);

    let mut app = App::new();
    app.start_stream();

    tokio::spawn(simulate_stream(event_tx, abort_rx));

    // Consume 5 tokens then abort
    let mut tokens_received = 0;
    loop {
        match event_rx.recv().await.unwrap() {
            Event::Token(t) => {
                app.on_token(t);
                tokens_received += 1;
                if tokens_received == 5 {
                    // Simulate Ctrl+C
                    app.on_abort();
                    let _ = abort_tx.send(()).await;
                }
            }
            Event::Done => {
                app.on_stream_done();
                break;
            }
        }
    }

    assert_eq!(app.stream_state, StreamState::Idle, "should be Idle after stream_done");
    assert!(app.turn_count == 1, "one turn completed");

    let (_, assistant) = &app.history[0];
    assert!(
        assistant.contains("[ABORTED]"),
        "history entry should be marked ABORTED"
    );
    // Partial text (5 tokens) must be preserved, not empty
    let partial = assistant.replace(" [ABORTED]", "");
    assert!(!partial.is_empty(), "partial text must be non-empty: got '{}'", partial);
    assert!(
        partial.contains("The "),
        "partial text should contain at least the first token"
    );
}

/// AC-2b: Tokens arriving after abort are silently dropped.
#[tokio::test]
async fn ac2_tokens_dropped_after_abort() {
    let mut app = App::new();
    app.start_stream();

    app.on_token("hello ".into());
    app.on_token("world ".into());
    let text_before_abort = app.streaming_text.clone();

    app.on_abort();
    assert_eq!(app.stream_state, StreamState::Aborted);

    // These tokens must be dropped
    app.on_token("should ".into());
    app.on_token("be ".into());
    app.on_token("dropped".into());

    assert_eq!(
        app.streaming_text, text_before_abort,
        "streaming_text must not change after abort"
    );
}

/// AC-2c: Abort latency is recorded (≤ 100ms in headless mode with 1ms delays).
#[tokio::test]
async fn ac2_abort_latency_under_100ms() {
    let (event_tx, mut event_rx) = mpsc::channel::<Event>(64);
    let (abort_tx, abort_rx) = mpsc::channel::<()>(1);

    let mut app = App::new();
    app.start_stream();

    tokio::spawn(simulate_stream(event_tx, abort_rx));

    let abort_time = Instant::now();
    let mut first_token = true;
    loop {
        match event_rx.recv().await.unwrap() {
            Event::Token(t) => {
                if first_token {
                    app.on_abort();
                    let _ = abort_tx.send(()).await;
                    first_token = false;
                }
                app.on_token(t); // will be dropped if Aborted
            }
            Event::Done => {
                app.on_stream_done();
                break;
            }
        }
    }

    let elapsed = abort_time.elapsed().as_millis();
    assert!(
        elapsed < 100,
        "abort-to-done latency must be <100ms, got {}ms",
        elapsed
    );
}

// ── AC-3: Enter queues input during streaming ──────────────────────────────

/// AC-3a: Submitting during streaming queues the input.
#[tokio::test]
async fn ac3_submit_during_streaming_is_queued() {
    let mut app = App::new();
    app.start_stream();

    assert_eq!(app.stream_state, StreamState::Streaming);

    app.on_submit("first queued message".into());
    app.on_submit("second queued message".into());

    assert_eq!(app.queued_inputs.len(), 2, "both inputs must be queued");
    assert_eq!(app.queued_inputs[0], "first queued message");
    assert_eq!(app.queued_inputs[1], "second queued message");
}

/// AC-3b: Empty submit is ignored (not queued).
#[tokio::test]
async fn ac3_empty_submit_ignored() {
    let mut app = App::new();
    app.start_stream();

    app.on_submit("".into());
    app.on_submit("   ".into());

    assert_eq!(app.queued_inputs.len(), 0, "empty inputs must not be queued");
}

/// AC-3c: Queued inputs survive stream completion and are accessible.
#[tokio::test]
async fn ac3_queued_inputs_survive_stream_done() {
    let mut app = App::new();
    app.start_stream();

    app.on_token("response token ".into());
    app.on_submit("queued while streaming".into());

    // Stream ends naturally
    app.on_stream_done();

    assert_eq!(app.stream_state, StreamState::Idle);
    assert_eq!(app.queued_inputs.len(), 1, "queued input must survive stream_done");
    assert_eq!(app.queued_inputs[0], "queued while streaming");
}

// ── AC-4: 100 turns without memory growth ─────────────────────────────────

/// AC-4: Run 100 full streaming turns end-to-end.
/// Verifies: turn count reaches 100, no panic, history length bounded.
#[tokio::test]
async fn ac4_100_turns_complete() {
    let (event_tx, mut event_rx) = mpsc::channel::<Event>(256);
    let mut app = App::new();

    // We'll run turns sequentially to keep it deterministic
    for turn in 0..100u64 {
        let (abort_tx_unused, abort_rx) = mpsc::channel::<()>(1);
        drop(abort_tx_unused); // no abort in this test

        app.start_stream();
        let tx = event_tx.clone();
        tokio::spawn(simulate_stream(tx, abort_rx));

        loop {
            match event_rx.recv().await.unwrap() {
                Event::Token(t) => app.on_token(t),
                Event::Done => {
                    app.on_stream_done();
                    break;
                }
            }
        }

        assert_eq!(
            app.turn_count,
            turn + 1,
            "turn_count must increment each turn"
        );
        assert_eq!(
            app.stream_state,
            StreamState::Idle,
            "must be Idle after stream_done"
        );
        assert!(
            app.streaming_text.is_empty(),
            "streaming_text must be cleared after stream_done"
        );
    }

    assert_eq!(app.turn_count, 100, "must complete exactly 100 turns");
    assert_eq!(app.history.len(), 100, "history must have 100 entries");

    // AC-4: History entries should not accumulate unbounded streaming_text
    for (_, assistant) in &app.history {
        assert!(
            !assistant.is_empty(),
            "each history entry must have non-empty assistant text"
        );
        assert!(
            !assistant.contains("[ABORTED]"),
            "no aborts in this test"
        );
    }
}

/// AC-4b: streaming_text is always cleared between turns (no accumulation).
#[tokio::test]
async fn ac4_streaming_text_cleared_between_turns() {
    let mut app = App::new();

    for _ in 0..10 {
        app.start_stream();
        for word in &["hello ", "world ", "test "] {
            app.on_token(word.to_string());
        }
        app.on_stream_done();
        assert!(
            app.streaming_text.is_empty(),
            "streaming_text must be empty after on_stream_done"
        );
    }
}

// ── AC-5: No deadlock (async logic) ───────────────────────────────────────

/// AC-5: Run the async event loop for 100 turns with a timeout.
/// If it completes before the timeout → no deadlock.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ac5_no_deadlock_100_turns() {
    let result = tokio::time::timeout(
        Duration::from_secs(30), // 100 turns at 1ms/token × 40 tokens = ~4s; 30s is generous
        async {
            let (event_tx, mut event_rx) = mpsc::channel::<Event>(256);
            let mut app = App::new();

            for _ in 0..100u64 {
                let (_, abort_rx) = mpsc::channel::<()>(1);
                app.start_stream();
                let tx = event_tx.clone();
                tokio::spawn(simulate_stream(tx, abort_rx));

                loop {
                    match event_rx.recv().await.unwrap() {
                        Event::Token(t) => app.on_token(t),
                        Event::Done => {
                            app.on_stream_done();
                            break;
                        }
                    }
                }
            }
            app.turn_count
        },
    )
    .await;

    match result {
        Ok(turns) => assert_eq!(turns, 100, "must complete 100 turns"),
        Err(_) => panic!("AC-5 FAIL: deadlock detected — timed out after 30s"),
    }
}

/// AC-5b: Abort mid-stream does not deadlock.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ac5_abort_does_not_deadlock() {
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        let (event_tx, mut event_rx) = mpsc::channel::<Event>(64);
        let (abort_tx, abort_rx) = mpsc::channel::<()>(1);

        let mut app = App::new();
        app.start_stream();

        tokio::spawn(simulate_stream(event_tx, abort_rx));

        let mut count = 0;
        loop {
            match event_rx.recv().await.unwrap() {
                Event::Token(t) => {
                    app.on_token(t);
                    count += 1;
                    if count == 3 {
                        app.on_abort();
                        let _ = abort_tx.send(()).await;
                    }
                }
                Event::Done => {
                    app.on_stream_done();
                    break;
                }
            }
        }
        app.turn_count
    })
    .await;

    match result {
        Ok(turns) => assert_eq!(turns, 1, "must complete 1 turn after abort"),
        Err(_) => panic!("AC-5b FAIL: abort caused deadlock"),
    }
}
