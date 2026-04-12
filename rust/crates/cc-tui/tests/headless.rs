//! Headless tests for cc-tui — ports the TUI spike acceptance criteria (AC-2 through AC-5)
//! to the production App + update() API.
//!
//! Spike test file: rust/spikes/tui/tests/headless.rs
//! Production equivalence:
//!   spike App::new()         → App::new(session_id, model)
//!   spike app.start_stream() → app.start_stream()
//!   spike app.on_token(t)    → app.on_token(&t)
//!   spike app.on_abort()     → update(&mut app, AppAction::Abort, &uctx)
//!   spike app.on_stream_done()
//!                            → update(&mut app, AppAction::TurnComplete { usage }, &uctx)
//!   spike app.on_submit(t)   → app.input = t; update(&mut app, AppAction::Submit, &uctx)
//!   spike StreamState::Idle  → AppMode::Input
//!   spike app.history        → app.transcript (TranscriptItem::AssistantText entries)
//!   spike app.turn_count     → app.status.turn_count
//!   spike app.queued_inputs  → app.queued

use cc_core::{AppEvent, Usage};
use cc_tui::{
    update, App, AppAction, AppMode, CommandContext, CommandRegistry, TranscriptItem, UpdateContext,
    UpdateResult,
};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

fn test_ctx() -> (CommandRegistry, CommandContext) {
    (
        CommandRegistry::empty(),
        CommandContext::new("0.1.0", "test-model"),
    )
}

fn zero_usage() -> Usage {
    Usage::default()
}

// ── AC-2: Abort preserves partial text ───────────────────────────────────────

/// AC-2a: After abort mid-stream, partial text appears in transcript with an
/// aborted marker, and streaming_text is cleared.
#[test]
fn ac2_partial_text_preserved_after_abort() {
    let mut app = App::new("sess".into(), "test-model".into());
    let (reg, ctx) = test_ctx();
    let uctx = UpdateContext { commands: &reg, command_ctx: &ctx };

    app.start_stream();
    app.on_token("The ");
    app.on_token("quick ");
    app.on_token("brown ");
    // 3 tokens received → abort
    update(&mut app, AppAction::Abort, &uctx);

    assert_eq!(app.mode, AppMode::Input, "mode must be Input after abort");
    assert!(app.streaming_text.is_empty(), "streaming_text must be cleared after abort");

    match app.transcript.last() {
        Some(TranscriptItem::AssistantText(t)) => {
            assert!(t.contains("The quick brown"), "partial text must be preserved: got '{t}'");
            assert!(t.contains("aborted"), "aborted marker must be present: got '{t}'");
        }
        other => panic!("expected AssistantText with aborted marker, got {other:?}"),
    }
}

/// AC-2b: streaming_text is empty immediately after abort (the partial text was
/// taken and committed to transcript, not lost).
#[test]
fn ac2_streaming_text_cleared_after_abort() {
    let mut app = App::new("s".into(), "m".into());
    let (reg, ctx) = test_ctx();
    let uctx = UpdateContext { commands: &reg, command_ctx: &ctx };

    app.start_stream();
    app.on_token("hello ");
    app.on_token("world");
    assert_eq!(app.streaming_text, "hello world", "pre-condition: text accumulated");

    update(&mut app, AppAction::Abort, &uctx);
    assert!(
        app.streaming_text.is_empty(),
        "streaming_text must be empty after abort, got '{}'",
        app.streaming_text
    );
}

/// AC-2c: Abort-path state transitions are fast — 100 abort cycles in <<100ms.
/// (Headless proxy for the ≤100ms latency requirement; the real latency is
/// measured at the crossterm level in integration tests.)
#[test]
fn ac2_abort_state_transition_is_fast() {
    let start = Instant::now();
    for _ in 0..100 {
        let mut app = App::new("s".into(), "m".into());
        let (reg, ctx) = test_ctx();
        let uctx = UpdateContext { commands: &reg, command_ctx: &ctx };
        app.start_stream();
        for i in 0..10u32 {
            app.on_token(&format!("token{i} "));
        }
        update(&mut app, AppAction::Abort, &uctx);
        assert!(app.streaming_text.is_empty());
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed.as_millis() < 100,
        "100 abort-cycle state transitions must take <100ms, got {elapsed:?}"
    );
}

// ── AC-3: Submit during streaming queues input ────────────────────────────────

/// AC-3a: While in Streaming mode, Submit queues the input (does not submit
/// to the engine) and returns UpdateResult::Continue.
#[test]
fn ac3_submit_during_streaming_is_queued() {
    let mut app = App::new("s".into(), "m".into());
    let (reg, ctx) = test_ctx();
    let uctx = UpdateContext { commands: &reg, command_ctx: &ctx };

    app.start_stream();
    assert_eq!(app.mode, AppMode::Streaming);

    app.input = "first queued message".into();
    let r1 = update(&mut app, AppAction::Submit, &uctx);
    assert!(
        matches!(r1, UpdateResult::Continue),
        "submit during streaming must return Continue, got {r1:?}"
    );
    assert_eq!(app.queued.len(), 1);
    assert_eq!(app.queued[0], "first queued message");

    app.input = "second queued message".into();
    let r2 = update(&mut app, AppAction::Submit, &uctx);
    assert!(matches!(r2, UpdateResult::Continue));
    assert_eq!(app.queued.len(), 2);
    assert_eq!(app.queued[1], "second queued message");
}

/// AC-3b: Empty and whitespace-only submits are ignored (not queued).
#[test]
fn ac3_empty_submit_ignored() {
    let mut app = App::new("s".into(), "m".into());
    let (reg, ctx) = test_ctx();
    let uctx = UpdateContext { commands: &reg, command_ctx: &ctx };

    app.start_stream();

    app.input = "".into();
    update(&mut app, AppAction::Submit, &uctx);
    assert_eq!(app.queued.len(), 0, "empty input must not be queued");

    app.input = "   ".into();
    update(&mut app, AppAction::Submit, &uctx);
    assert_eq!(app.queued.len(), 0, "whitespace-only input must not be queued");
}

/// AC-3c: Queued inputs survive stream completion and are drained by TurnComplete.
#[test]
fn ac3_queued_inputs_drained_on_turn_complete() {
    let mut app = App::new("s".into(), "m".into());
    let (reg, ctx) = test_ctx();
    let uctx = UpdateContext { commands: &reg, command_ctx: &ctx };

    app.start_stream();
    app.on_token("response token ");

    app.input = "queued while streaming".into();
    update(&mut app, AppAction::Submit, &uctx);
    assert_eq!(app.queued.len(), 1, "message queued during stream");

    let result = update(&mut app, AppAction::TurnComplete { usage: zero_usage() }, &uctx);
    assert!(
        matches!(result, UpdateResult::SubmitToEngine(ref t) if t == "queued while streaming"),
        "TurnComplete must drain queue and return SubmitToEngine, got {result:?}"
    );
    assert_eq!(app.queued.len(), 0, "queue must be empty after drain");
}

// ── AC-4: 100 turns without memory growth ─────────────────────────────────────

/// AC-4: Run 100 full streaming turns. Verify turn_count reaches 100,
/// streaming_text is cleared after each turn, mode returns to Input.
#[test]
fn ac4_100_turns_complete() {
    let mut app = App::new("s".into(), "m".into());
    let (reg, ctx) = test_ctx();
    let uctx = UpdateContext { commands: &reg, command_ctx: &ctx };
    let tokens = ["The ", "quick ", "brown ", "fox ", "jumps "];

    for turn in 0..100u32 {
        app.start_stream();
        for &t in &tokens {
            app.on_token(t);
        }
        update(
            &mut app,
            AppAction::TurnComplete {
                usage: Usage { input_tokens: 10, output_tokens: 5, ..Default::default() },
            },
            &uctx,
        );

        assert_eq!(app.status.turn_count, turn + 1, "turn_count must increment each turn");
        assert_eq!(app.mode, AppMode::Input, "mode must be Input after TurnComplete");
        assert!(app.streaming_text.is_empty(), "streaming_text must be cleared after TurnComplete");
    }

    assert_eq!(app.status.turn_count, 100, "must complete exactly 100 turns");

    let assistant_count = app
        .transcript
        .iter()
        .filter(|i| matches!(i, TranscriptItem::AssistantText(_)))
        .count();
    assert_eq!(assistant_count, 100, "transcript must have exactly 100 AssistantText entries");

    // Each AssistantText entry must be non-empty and not contain abort marker.
    for item in &app.transcript {
        if let TranscriptItem::AssistantText(text) = item {
            assert!(!text.is_empty(), "each AssistantText must be non-empty");
            assert!(!text.contains("aborted"), "no aborts in this test");
        }
    }
}

/// AC-4b: streaming_text is always cleared between turns (no accumulation).
#[test]
fn ac4_streaming_text_cleared_between_turns() {
    let mut app = App::new("s".into(), "m".into());
    let (reg, ctx) = test_ctx();
    let uctx = UpdateContext { commands: &reg, command_ctx: &ctx };

    for _ in 0..10 {
        app.start_stream();
        app.on_token("hello ");
        app.on_token("world");
        update(&mut app, AppAction::TurnComplete { usage: zero_usage() }, &uctx);
        assert!(
            app.streaming_text.is_empty(),
            "streaming_text must be empty after TurnComplete"
        );
    }
}

// ── AC-5: No deadlock ─────────────────────────────────────────────────────────

/// AC-5: Drive the App state machine through 100 turns via an mpsc channel
/// (simulating the engine→TUI event path). Must complete within 30s.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ac5_no_deadlock_100_turns() {
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        let (tx, mut rx) = mpsc::channel::<AppEvent>(256);
        let mut app = App::new("s".into(), "m".into());
        let (reg, ctx) = test_ctx();
        let uctx = UpdateContext { commands: &reg, command_ctx: &ctx };

        for _ in 0..100u64 {
            let tx2 = tx.clone();
            tokio::spawn(async move {
                for i in 0..5u64 {
                    let _ = tx2.send(AppEvent::StreamDelta(format!("tok{i} "))).await;
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                let _ = tx2
                    .send(AppEvent::TurnComplete {
                        usage: Usage { input_tokens: 10, output_tokens: 5, ..Default::default() },
                    })
                    .await;
            });

            loop {
                match rx.recv().await.expect("channel closed unexpectedly") {
                    AppEvent::StreamDelta(d) => {
                        update(&mut app, AppAction::StreamDelta(d), &uctx);
                    }
                    AppEvent::TurnComplete { usage } => {
                        update(&mut app, AppAction::TurnComplete { usage }, &uctx);
                        break;
                    }
                    _ => {}
                }
            }
        }

        app.status.turn_count
    })
    .await;

    match result {
        Ok(n) => assert_eq!(n, 100, "must complete exactly 100 turns"),
        Err(_) => panic!("AC-5 FAIL: deadlock detected — timed out after 30s"),
    }
}

/// AC-5b: Abort mid-stream completes without deadlock. Simulates Abort arriving
/// while StreamDelta events are still in flight, followed by TurnComplete.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ac5_abort_does_not_deadlock() {
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        let (tx, mut rx) = mpsc::channel::<AppEvent>(64);
        let mut app = App::new("s".into(), "m".into());
        let (reg, ctx) = test_ctx();
        let uctx = UpdateContext { commands: &reg, command_ctx: &ctx };

        // Engine sends 40 deltas then TurnComplete (as if cancellation is slightly delayed).
        let tx2 = tx.clone();
        tokio::spawn(async move {
            for i in 0..40u64 {
                let _ = tx2.send(AppEvent::StreamDelta(format!("t{i} "))).await;
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            let _ = tx2
                .send(AppEvent::TurnComplete { usage: Usage::default() })
                .await;
        });

        app.start_stream();
        let mut delta_count = 0u64;
        loop {
            match rx.recv().await.expect("channel closed") {
                AppEvent::StreamDelta(d) => {
                    update(&mut app, AppAction::StreamDelta(d), &uctx);
                    delta_count += 1;
                    if delta_count == 3 {
                        // Simulate Ctrl+C after 3 tokens.
                        update(&mut app, AppAction::Abort, &uctx);
                    }
                }
                AppEvent::TurnComplete { usage } => {
                    // TurnComplete arrives even after abort (async cancellation delay).
                    update(&mut app, AppAction::TurnComplete { usage }, &uctx);
                    break;
                }
                _ => {}
            }
        }
    })
    .await;

    match result {
        Ok(()) => {} // Completed — no deadlock.
        Err(_) => panic!("AC-5b FAIL: abort caused deadlock — timed out after 5s"),
    }
}
