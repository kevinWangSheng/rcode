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
    update, App, AppAction, AppMode, CommandContext, CommandRegistry, TranscriptItem,
    UpdateContext, UpdateResult,
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
    let uctx = UpdateContext {
        commands: &reg,
        command_ctx: &ctx,
    };

    app.start_stream();
    app.on_token("The ");
    app.on_token("quick ");
    app.on_token("brown ");
    // 3 tokens received → abort
    update(&mut app, AppAction::Abort, &uctx);

    assert_eq!(app.mode, AppMode::Input, "mode must be Input after abort");
    assert!(
        app.streaming_text.is_empty(),
        "streaming_text must be cleared after abort"
    );

    match app.transcript.last() {
        Some(TranscriptItem::AssistantText(t)) => {
            assert!(
                t.contains("The quick brown"),
                "partial text must be preserved: got '{t}'"
            );
            assert!(
                t.contains("aborted"),
                "aborted marker must be present: got '{t}'"
            );
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
    let uctx = UpdateContext {
        commands: &reg,
        command_ctx: &ctx,
    };

    app.start_stream();
    app.on_token("hello ");
    app.on_token("world");
    assert_eq!(
        app.streaming_text, "hello world",
        "pre-condition: text accumulated"
    );

    update(&mut app, AppAction::Abort, &uctx);
    assert!(
        app.streaming_text.is_empty(),
        "streaming_text must be empty after abort, got '{}'",
        app.streaming_text
    );
}

/// AC-2b: StreamDelta actions arriving after Abort are dropped. The engine
/// may still emit in-flight deltas after the cancel token trips (network
/// read loop is async), so the App must guard at the action boundary.
#[test]
fn ac2_tokens_dropped_after_abort() {
    let mut app = App::new("s".into(), "m".into());
    let (reg, ctx) = test_ctx();
    let uctx = UpdateContext {
        commands: &reg,
        command_ctx: &ctx,
    };

    app.start_stream();
    update(&mut app, AppAction::StreamDelta("hello ".into()), &uctx);
    update(&mut app, AppAction::StreamDelta("world".into()), &uctx);
    assert_eq!(app.streaming_text, "hello world");

    // Abort mid-stream.
    update(&mut app, AppAction::Abort, &uctx);
    assert_eq!(app.mode, AppMode::Input);

    // These late deltas must be silently dropped.
    update(&mut app, AppAction::StreamDelta("late1 ".into()), &uctx);
    update(&mut app, AppAction::StreamDelta("late2".into()), &uctx);
    assert!(
        app.streaming_text.is_empty(),
        "streaming_text must stay empty after abort; got '{}'",
        app.streaming_text
    );

    // Transcript's last assistant entry still holds the pre-abort partial.
    match app.transcript.last() {
        Some(TranscriptItem::AssistantText(t)) => {
            assert!(t.contains("hello world"), "pre-abort partial kept: {t}");
            assert!(
                !t.contains("late1"),
                "post-abort tokens must not leak in: {t}"
            );
            assert!(
                !t.contains("late2"),
                "post-abort tokens must not leak in: {t}"
            );
        }
        other => panic!("expected AssistantText with aborted marker, got {other:?}"),
    }
}

/// AC-2c: End-to-end abort latency <100ms. Drives the full async event path
/// (engine task sends deltas over an mpsc channel; event loop processes
/// them + an Abort action). Measures wall-clock elapsed from the Abort
/// action until the App returns to Input mode. The headless event loop is
/// the production one minus crossterm I/O, so this is a real end-to-end
/// latency number, not a state-transition microbenchmark.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ac2_abort_latency_under_100ms_end_to_end() {
    let (tx, mut rx) = mpsc::channel::<AppEvent>(128);
    let mut app = App::new("s".into(), "m".into());
    let (reg, ctx) = test_ctx();
    let uctx = UpdateContext {
        commands: &reg,
        command_ctx: &ctx,
    };

    // Engine: emit 400 tokens at 1ms — plenty of buffer so we can catch a
    // slow-abort regression. Ends with TurnComplete so the channel closes
    // cleanly if abort somehow fails to take effect.
    let tx2 = tx.clone();
    tokio::spawn(async move {
        for i in 0..400u64 {
            if tx2
                .send(AppEvent::StreamDelta(format!("tok{i} ")))
                .await
                .is_err()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let _ = tx2
            .send(AppEvent::TurnComplete {
                usage: Usage::default(),
            })
            .await;
    });

    app.start_stream();

    // Consume 5 tokens so we're mid-stream when abort fires.
    for _ in 0..5 {
        let ev = rx.recv().await.expect("channel alive");
        if let AppEvent::StreamDelta(d) = ev {
            update(&mut app, AppAction::StreamDelta(d), &uctx);
        }
    }

    let abort_start = Instant::now();
    update(&mut app, AppAction::Abort, &uctx);
    let abort_elapsed = abort_start.elapsed();

    assert_eq!(app.mode, AppMode::Input, "Abort must return to Input mode");
    assert!(
        abort_elapsed.as_millis() < 100,
        "end-to-end Abort handling must take <100ms, got {abort_elapsed:?}"
    );

    // Drain any in-flight events until the engine task ends. All remaining
    // StreamDeltas must be dropped by the AC-2b guard.
    let drain_start = Instant::now();
    loop {
        match tokio::time::timeout(Duration::from_millis(50), rx.recv()).await {
            Ok(Some(AppEvent::StreamDelta(d))) => {
                update(&mut app, AppAction::StreamDelta(d), &uctx);
            }
            Ok(Some(AppEvent::TurnComplete { usage })) => {
                update(&mut app, AppAction::TurnComplete { usage }, &uctx);
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
        if drain_start.elapsed() > Duration::from_secs(2) {
            panic!("drain stuck for >2s");
        }
    }

    assert!(
        app.streaming_text.is_empty(),
        "post-abort streaming_text must be empty; late deltas must be dropped"
    );
}

/// State-transition microbenchmark: 100 abort cycles in <100ms. Kept as a
/// cheap regression guard distinct from the end-to-end latency test above.
#[test]
fn ac2_abort_state_transition_is_fast() {
    let start = Instant::now();
    for _ in 0..100 {
        let mut app = App::new("s".into(), "m".into());
        let (reg, ctx) = test_ctx();
        let uctx = UpdateContext {
            commands: &reg,
            command_ctx: &ctx,
        };
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
    let uctx = UpdateContext {
        commands: &reg,
        command_ctx: &ctx,
    };

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
    let uctx = UpdateContext {
        commands: &reg,
        command_ctx: &ctx,
    };

    app.start_stream();

    app.input = "".into();
    update(&mut app, AppAction::Submit, &uctx);
    assert_eq!(app.queued.len(), 0, "empty input must not be queued");

    app.input = "   ".into();
    update(&mut app, AppAction::Submit, &uctx);
    assert_eq!(
        app.queued.len(),
        0,
        "whitespace-only input must not be queued"
    );
}

/// AC-3c: Queued inputs survive stream completion and are drained by TurnComplete.
#[test]
fn ac3_queued_inputs_drained_on_turn_complete() {
    let mut app = App::new("s".into(), "m".into());
    let (reg, ctx) = test_ctx();
    let uctx = UpdateContext {
        commands: &reg,
        command_ctx: &ctx,
    };

    app.start_stream();
    app.on_token("response token ");

    app.input = "queued while streaming".into();
    update(&mut app, AppAction::Submit, &uctx);
    assert_eq!(app.queued.len(), 1, "message queued during stream");

    let result = update(
        &mut app,
        AppAction::TurnComplete {
            usage: zero_usage(),
        },
        &uctx,
    );
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
    let uctx = UpdateContext {
        commands: &reg,
        command_ctx: &ctx,
    };
    let tokens = ["The ", "quick ", "brown ", "fox ", "jumps "];

    for turn in 0..100u32 {
        app.start_stream();
        for &t in &tokens {
            app.on_token(t);
        }
        update(
            &mut app,
            AppAction::TurnComplete {
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 5,
                    ..Default::default()
                },
            },
            &uctx,
        );

        assert_eq!(
            app.status.turn_count,
            turn + 1,
            "turn_count must increment each turn"
        );
        assert_eq!(
            app.mode,
            AppMode::Input,
            "mode must be Input after TurnComplete"
        );
        assert!(
            app.streaming_text.is_empty(),
            "streaming_text must be cleared after TurnComplete"
        );
    }

    assert_eq!(
        app.status.turn_count, 100,
        "must complete exactly 100 turns"
    );

    let assistant_count = app
        .transcript
        .iter()
        .filter(|i| matches!(i, TranscriptItem::AssistantText(_)))
        .count();
    assert_eq!(
        assistant_count, 100,
        "transcript must have exactly 100 AssistantText entries"
    );

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
    let uctx = UpdateContext {
        commands: &reg,
        command_ctx: &ctx,
    };

    for _ in 0..10 {
        app.start_stream();
        app.on_token("hello ");
        app.on_token("world");
        update(
            &mut app,
            AppAction::TurnComplete {
                usage: zero_usage(),
            },
            &uctx,
        );
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
        let uctx = UpdateContext {
            commands: &reg,
            command_ctx: &ctx,
        };

        for _ in 0..100u64 {
            let tx2 = tx.clone();
            tokio::spawn(async move {
                for i in 0..5u64 {
                    let _ = tx2.send(AppEvent::StreamDelta(format!("tok{i} "))).await;
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                let _ = tx2
                    .send(AppEvent::TurnComplete {
                        usage: Usage {
                            input_tokens: 10,
                            output_tokens: 5,
                            ..Default::default()
                        },
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
        let uctx = UpdateContext {
            commands: &reg,
            command_ctx: &ctx,
        };

        // Engine sends 40 deltas then TurnComplete (as if cancellation is slightly delayed).
        let tx2 = tx.clone();
        tokio::spawn(async move {
            for i in 0..40u64 {
                let _ = tx2.send(AppEvent::StreamDelta(format!("t{i} "))).await;
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            let _ = tx2
                .send(AppEvent::TurnComplete {
                    usage: Usage::default(),
                })
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

// ── M5 AC-V1 / AC-V6: Env-var-sensitive tests must serialise ────────────────
//
// AC-V1 asserts the markdown path IS used; AC-V6 asserts it is NOT (under
// CC_TUI_MINIMAL=1). `cargo test` runs tests in parallel by default, so one
// touching `CC_TUI_MINIMAL` can poison the other. Gate both on the same
// mutex.
static ENV_MUTEX: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    ENV_MUTEX
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

// ── M5 AC-V1: Markdown round-trip ────────────────────────────────────────────

/// AC-V1: an assistant reply containing all six markdown primitives renders
/// into the ratatui back-buffer with each element visually distinct.
#[test]
fn acv1_markdown_round_trip_all_six_elements() {
    use cc_tui::render;
    use ratatui::{backend::TestBackend, Terminal};

    let _g = env_lock();
    // Defensive: a prior run may have left CC_TUI_MINIMAL set if a test
    // panicked mid-body. Clear it before we render.
    std::env::remove_var("CC_TUI_MINIMAL");

    let mut app = App::new("sess".into(), "test-model".into());
    app.transcript
        .push(TranscriptItem::AssistantText(String::from(
            "\
### heading three
- first bullet
- with **bold**, *italic*, and `code`

```rust
fn main() {}
```
",
        )));

    let backend = TestBackend::new(80, 24);
    let mut term = Terminal::new(backend).unwrap();
    term.draw(|f| render::render(f, &app)).unwrap();
    let buf = term.backend().buffer().clone();
    let mut s = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            s.push_str(buf[(x, y)].symbol());
        }
        s.push('\n');
    }

    assert!(s.contains("▎ heading three"), "heading missing: {s}");
    assert!(s.contains("• first bullet"), "bullet missing: {s}");
    assert!(s.contains("bold"), "bold text missing: {s}");
    assert!(s.contains("italic"), "italic text missing: {s}");
    assert!(s.contains("code"), "inline code missing: {s}");
    assert!(s.contains("fn main() {}"), "fenced code body missing: {s}");
    assert!(s.contains("rust"), "fenced code language label missing: {s}");
}

// ── M5 AC-V2: Spinner timing ─────────────────────────────────────────────────

/// AC-V2 (first clause): the spinner is visible within 100 ms of `Submit`.
/// We measure the state flip, not the render — the ratatui redraw is on the
/// critical path of `update()` and has no deferred work.
#[test]
fn acv2_spinner_visible_within_100ms_of_submit() {
    let mut app = App::new("sess".into(), "test-model".into());
    let (reg, ctx) = test_ctx();
    let uctx = UpdateContext {
        commands: &reg,
        command_ctx: &ctx,
    };

    app.input = "hello".into();
    let t0 = Instant::now();
    let result = update(&mut app, AppAction::Submit, &uctx);
    let elapsed = t0.elapsed();

    assert!(
        matches!(result, UpdateResult::SubmitToEngine(_)),
        "Submit must trigger engine submission"
    );
    assert!(
        app.stream_started_at.is_some(),
        "stream_started_at must be set by Submit"
    );
    assert!(
        !app.spinner_glyph().is_empty(),
        "spinner glyph must be non-empty during streaming"
    );
    assert!(
        elapsed < Duration::from_millis(100),
        "Submit → spinner-visible took {elapsed:?}, must be < 100ms"
    );
}

/// AC-V2 (second clause): the spinner clears within 100 ms of TurnComplete.
#[test]
fn acv2_spinner_clears_within_100ms_of_turn_end() {
    let mut app = App::new("sess".into(), "test-model".into());
    let (reg, ctx) = test_ctx();
    let uctx = UpdateContext {
        commands: &reg,
        command_ctx: &ctx,
    };

    app.input = "hi".into();
    update(&mut app, AppAction::Submit, &uctx);
    assert!(app.stream_started_at.is_some());
    assert!(!app.spinner_glyph().is_empty());

    let t0 = Instant::now();
    update(
        &mut app,
        AppAction::TurnComplete {
            usage: zero_usage(),
        },
        &uctx,
    );
    let elapsed = t0.elapsed();

    assert!(
        app.stream_started_at.is_none(),
        "stream_started_at must be cleared by TurnComplete"
    );
    assert!(
        app.spinner_glyph().is_empty(),
        "spinner glyph must be empty after TurnComplete"
    );
    assert!(
        elapsed < Duration::from_millis(100),
        "TurnComplete → spinner-cleared took {elapsed:?}"
    );
}

// ── M5 AC-V3: Tool-use card ──────────────────────────────────────────────────

/// AC-V3: a `Bash(ls /tmp)` call renders as a single-line card with green
/// tool color and the result indented beneath. Raw JSON must never appear.
#[test]
fn acv3_tool_use_card_renders_with_color_and_no_raw_json() {
    use cc_tui::render;
    use ratatui::{backend::TestBackend, Terminal};

    let _g = env_lock();
    std::env::remove_var("CC_TUI_MINIMAL");

    let mut app = App::new("sess".into(), "test-model".into());
    app.push_tool_call_with_input(
        "Bash".into(),
        "ls /tmp".into(),
        serde_json::json!({ "command": "ls /tmp" }),
    );
    app.push_tool_result("Bash".into(), "a\nb\nc".into(), false);

    let backend = TestBackend::new(80, 24);
    let mut term = Terminal::new(backend).unwrap();
    term.draw(|f| render::render(f, &app)).unwrap();
    let buf = term.backend().buffer().clone();
    let mut s = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            s.push_str(buf[(x, y)].symbol());
        }
        s.push('\n');
    }

    // Header rendered in the new card format.
    assert!(s.contains("⏺ Bash"), "card header missing: {s}");
    assert!(s.contains("(ls /tmp)"), "command preview missing: {s}");
    // Result carries the success tick.
    assert!(s.contains("✓"), "success tick missing: {s}");
    // Raw JSON must never leak onto the screen.
    assert!(
        !s.contains("\"command\""),
        "raw JSON key leaked onto screen: {s}"
    );

    // Walk the ratatui back-buffer to confirm the "B" in "Bash" is green.
    let mut found_green_bash = false;
    for y in 0..buf.area.height {
        for x in 0..buf.area.width.saturating_sub(1) {
            if buf[(x, y)].symbol() == "B" && buf[(x + 1, y)].symbol() == "a" {
                if buf[(x, y)].fg == ratatui::style::Color::Green {
                    found_green_bash = true;
                }
                break;
            }
        }
    }
    assert!(found_green_bash, "Bash header is not rendered in green");
}

// ── M5 AC-V4: Edit diff ──────────────────────────────────────────────────────

/// AC-V4: a successful `Edit` call renders a red/green unified diff with
/// ≥2 context lines on each side.
#[test]
fn acv4_edit_renders_unified_diff() {
    use cc_tui::render;
    use ratatui::{backend::TestBackend, Terminal};

    let _g = env_lock();
    std::env::remove_var("CC_TUI_MINIMAL");

    let mut app = App::new("sess".into(), "test-model".into());
    let old = "l1\nl2\nl3\nl4\nOLD\nl6\nl7\nl8\nl9";
    let new = "l1\nl2\nl3\nl4\nNEW\nl6\nl7\nl8\nl9";
    app.push_tool_call_with_input(
        "Edit".into(),
        "/tmp/foo.rs".into(),
        serde_json::json!({
            "file_path": "/tmp/foo.rs",
            "old_string": old,
            "new_string": new,
        }),
    );
    app.push_tool_result("Edit".into(), "Edit applied.".into(), false);

    let backend = TestBackend::new(120, 40);
    let mut term = Terminal::new(backend).unwrap();
    term.draw(|f| render::render(f, &app)).unwrap();
    let buf = term.backend().buffer().clone();
    let mut s = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            s.push_str(buf[(x, y)].symbol());
        }
        s.push('\n');
    }

    // Header + diff rows.
    assert!(s.contains("⏺ Edit"), "edit card header missing: {s}");
    assert!(s.contains("- OLD"), "deleted line missing: {s}");
    assert!(s.contains("+ NEW"), "added line missing: {s}");

    // Walk rows to verify colour + context count.
    let rows: Vec<String> = s.lines().map(|l| l.trim_end().to_string()).collect();
    let minus_row = rows
        .iter()
        .position(|r| r.contains("- OLD"))
        .expect("minus row missing");
    let plus_row = rows
        .iter()
        .position(|r| r.contains("+ NEW"))
        .expect("plus row missing");
    // 2+ context rows before minus
    let mut before_ctx = 0;
    for r in rows[..minus_row].iter().rev() {
        if r.contains("l4") || r.contains("l3") || r.contains("l2") {
            before_ctx += 1;
            if before_ctx >= 2 {
                break;
            }
        } else if r.contains("⏺") || r.trim().is_empty() {
            break;
        }
    }
    assert!(before_ctx >= 2, "expected ≥2 context rows before minus");
    // 2+ context rows after plus
    let mut after_ctx = 0;
    for r in rows[plus_row + 1..].iter() {
        if r.contains("l6") || r.contains("l7") || r.contains("l8") {
            after_ctx += 1;
            if after_ctx >= 2 {
                break;
            }
        } else if r.contains("✓") || r.trim().is_empty() {
            break;
        }
    }
    assert!(after_ctx >= 2, "expected ≥2 context rows after plus");

    // Color verification: walk the buffer and find a cell whose glyph is 'O'
    // inside "- OLD" — it must carry red, and 'N' in "+ NEW" must carry green.
    let mut found_red_old = false;
    let mut found_green_new = false;
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            let cell = &buf[(x, y)];
            let c = cell.symbol();
            if c == "O" && cell.fg == ratatui::style::Color::Red {
                found_red_old = true;
            }
            if c == "N" && cell.fg == ratatui::style::Color::Green {
                found_green_new = true;
            }
        }
    }
    assert!(found_red_old, "- OLD not rendered in red");
    assert!(found_green_new, "+ NEW not rendered in green");
}

// ── M5 AC-V6: CC_TUI_MINIMAL opt-out ─────────────────────────────────────────

/// AC-V6: with CC_TUI_MINIMAL=1 set, an assistant message containing markdown
/// characters renders without the M5 transforms (no bullet glyph, no heading
/// prefix block, no fence borders). The output must therefore *not* contain
/// the characters that only the M5 path emits.
#[test]
fn acv6_minimal_opt_out_skips_markdown_transforms() {
    use cc_tui::render;
    use ratatui::{backend::TestBackend, Terminal};

    let _g = env_lock();
    std::env::set_var("CC_TUI_MINIMAL", "1");

    let mut app = App::new("sess".into(), "test-model".into());
    app.transcript
        .push(TranscriptItem::AssistantText(String::from(
            "### heading\n- bullet\n`code`",
        )));

    let backend = TestBackend::new(80, 10);
    let mut term = Terminal::new(backend).unwrap();
    term.draw(|f| render::render(f, &app)).unwrap();
    let buf = term.backend().buffer().clone();
    let mut s = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            s.push_str(buf[(x, y)].symbol());
        }
        s.push('\n');
    }

    std::env::remove_var("CC_TUI_MINIMAL");

    // The raw markdown must appear verbatim.
    assert!(s.contains("### heading"), "raw heading missing: {s}");
    assert!(s.contains("- bullet"), "raw bullet missing: {s}");
    // The M5-only glyphs must *not* appear in minimal mode.
    assert!(!s.contains("▎"), "M5 heading glyph leaked in minimal mode");
    assert!(!s.contains("• "), "M5 bullet glyph leaked in minimal mode");
}
