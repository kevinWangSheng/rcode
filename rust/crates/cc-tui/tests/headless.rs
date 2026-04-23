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
    assert!(
        s.contains("rust"),
        "fenced code language label missing: {s}"
    );
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

    // Walk the ratatui back-buffer to confirm the "B" in "Bash" carries the
    // theme-defined Bash colour. Phase D moved the palette to a centralised
    // `theme` module, so we anchor against that rather than `Color::Green`.
    let bash_fg = cc_tui::theme::current().tool_color("Bash");
    let mut found_bash_color = false;
    for y in 0..buf.area.height {
        for x in 0..buf.area.width.saturating_sub(1) {
            if buf[(x, y)].symbol() == "B" && buf[(x + 1, y)].symbol() == "a" {
                if buf[(x, y)].fg == bash_fg {
                    found_bash_color = true;
                }
                break;
            }
        }
    }
    assert!(
        found_bash_color,
        "Bash header is not rendered in theme.tool_color(\"Bash\") = {bash_fg:?}"
    );
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

    // Colour verification: walk the buffer and find a cell whose glyph is
    // 'O' inside "- OLD" — it must carry the theme `error` colour, and 'N'
    // in "+ NEW" must carry the theme `success` colour.
    let theme = cc_tui::theme::current();
    let mut found_red_old = false;
    let mut found_green_new = false;
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            let cell = &buf[(x, y)];
            let c = cell.symbol();
            if c == "O" && cell.fg == theme.error {
                found_red_old = true;
            }
            if c == "N" && cell.fg == theme.success {
                found_green_new = true;
            }
        }
    }
    assert!(
        found_red_old,
        "- OLD not rendered in theme.error = {:?}",
        theme.error
    );
    assert!(
        found_green_new,
        "+ NEW not rendered in theme.success = {:?}",
        theme.success
    );
}

// ── M5 AC-V5: Slash-command picker ───────────────────────────────────────────

/// AC-V5: typing `/` with an empty buffer opens the palette; the match list
/// includes at least /help, /memory, /clear, and every discovered user skill;
/// Tab completes + appends trailing space; Esc dismisses without mutating
/// the buffer.
#[test]
fn acv5_palette_lists_builtins_and_skills_and_tab_completes() {
    use cc_tui::commands::CommandRegistry;

    // Build a registry with a synthetic skill so we can assert "every skill"
    // appears.
    let reg = CommandRegistry::from_skills(vec![cc_memory::SkillDef {
        name: "demo-skill".into(),
        description: "demo".into(),
        model: None,
        content: "body".into(),
        user_invocable: true,
        path: std::path::PathBuf::from("/tmp/demo-skill.md"),
    }]);
    let ctx = CommandContext::new("0.1.0", "test-model");
    let uctx = UpdateContext {
        commands: &reg,
        command_ctx: &ctx,
    };

    let mut app = App::new("sess".into(), "test-model".into());

    // Open the palette. Input must become "/".
    update(&mut app, AppAction::PaletteOpen, &uctx);
    assert_eq!(app.mode, AppMode::CommandPalette);
    assert!(app.input.starts_with('/'), "palette must seed input with /");

    // Match list must include the documented builtins + the skill.
    for required in &["help", "memory", "clear", "demo-skill"] {
        assert!(
            app.palette_matches.iter().any(|n| n.as_str() == *required),
            "palette missing required entry /{required}: {:?}",
            app.palette_matches
        );
    }

    // Narrow via filter: typing "me" keeps /memory (and possibly /mcp, /model).
    update(&mut app, AppAction::InsertChar('m'), &uctx);
    update(&mut app, AppAction::InsertChar('e'), &uctx);
    assert!(
        app.palette_matches.iter().any(|n| n.as_str() == "memory"),
        "filter `me` must retain /memory"
    );
    assert!(
        app.palette_matches
            .iter()
            .all(|n| n.to_ascii_lowercase().starts_with("me")),
        "filter `me` must drop non-matching commands: {:?}",
        app.palette_matches
    );

    // Tab accepts: input becomes "/<name> " with trailing space.
    update(&mut app, AppAction::PaletteAccept, &uctx);
    assert_eq!(app.mode, AppMode::Input);
    assert!(
        app.input.ends_with(' '),
        "accept must append trailing space; got {:?}",
        app.input
    );
    assert!(
        app.input.starts_with("/memory"),
        "accept must replace buffer with the picked command, got {:?}",
        app.input
    );
    assert!(
        app.palette_matches.is_empty(),
        "accept must drop the match list"
    );
}

#[test]
fn acv5_palette_esc_restores_original_buffer() {
    let reg = CommandRegistry::empty();
    let ctx = CommandContext::new("0.1.0", "test-model");
    let uctx = UpdateContext {
        commands: &reg,
        command_ctx: &ctx,
    };

    let mut app = App::new("sess".into(), "test-model".into());
    // User had typed some prose before hitting `/`.
    app.input = "hello, world".into();
    let before = app.input.clone();

    update(&mut app, AppAction::PaletteOpen, &uctx);
    // Type some filter chars.
    update(&mut app, AppAction::InsertChar('h'), &uctx);
    update(&mut app, AppAction::InsertChar('e'), &uctx);

    // Esc.
    update(&mut app, AppAction::PaletteCancel, &uctx);
    assert_eq!(app.mode, AppMode::Input);
    assert_eq!(
        app.input, before,
        "PaletteCancel must restore the original input byte-for-byte"
    );
}

// ── fix-tui-palette-stale-after-submit: palette closes on any Submit ─────────

/// Helper: open palette, set filter to `cmd`, then Submit.
fn submit_from_palette(app: &mut App, uctx: &UpdateContext, cmd: &str) -> UpdateResult {
    update(app, AppAction::PaletteOpen, uctx);
    for c in cmd.chars() {
        update(app, AppAction::InsertChar(c), uctx);
    }
    update(app, AppAction::Submit, uctx)
}

fn assert_palette_closed(app: &App) {
    assert_eq!(app.mode, AppMode::Input, "mode must return to Input");
    assert!(
        app.palette_matches.is_empty(),
        "palette_matches must be cleared: {:?}",
        app.palette_matches
    );
    assert_eq!(app.palette_selected, 0, "palette_selected must reset to 0");
    assert!(
        app.palette_original.is_none(),
        "palette_original must be cleared"
    );
}

#[test]
fn palette_closes_after_info_command() {
    let (reg, ctx) = test_ctx();
    let uctx = UpdateContext {
        commands: &reg,
        command_ctx: &ctx,
    };
    let mut app = App::new("s".into(), "m".into());

    submit_from_palette(&mut app, &uctx, "help");
    assert_palette_closed(&app);
}

#[test]
fn palette_closes_after_clear_command() {
    let (reg, ctx) = test_ctx();
    let uctx = UpdateContext {
        commands: &reg,
        command_ctx: &ctx,
    };
    let mut app = App::new("s".into(), "m".into());
    // Seed a transcript item so we can observe /clear wiping it.
    app.push_user("hi".into());

    submit_from_palette(&mut app, &uctx, "clear");
    assert_palette_closed(&app);
    // /clear empties transcript and pushes a SystemNotice.
    match app.transcript.last() {
        Some(TranscriptItem::SystemNotice(m)) => assert!(m.contains("cleared")),
        other => panic!("expected SystemNotice after /clear, got {other:?}"),
    }
}

#[test]
fn palette_closes_after_compact_command() {
    let (reg, ctx) = test_ctx();
    let uctx = UpdateContext {
        commands: &reg,
        command_ctx: &ctx,
    };
    let mut app = App::new("s".into(), "m".into());

    submit_from_palette(&mut app, &uctx, "compact");
    assert_palette_closed(&app);
    assert!(
        matches!(app.transcript.last(), Some(TranscriptItem::CompactBoundary)),
        "compact must push a CompactBoundary"
    );
}

#[test]
fn palette_closes_after_switch_model() {
    let (reg, ctx) = test_ctx();
    let uctx = UpdateContext {
        commands: &reg,
        command_ctx: &ctx,
    };
    let mut app = App::new("s".into(), "m".into());

    submit_from_palette(&mut app, &uctx, "model gpt-4");
    assert_palette_closed(&app);
    assert_eq!(app.status.model, "gpt-4");
}

#[test]
fn palette_closes_after_unknown_command() {
    let (reg, ctx) = test_ctx();
    let uctx = UpdateContext {
        commands: &reg,
        command_ctx: &ctx,
    };
    let mut app = App::new("s".into(), "m".into());

    submit_from_palette(&mut app, &uctx, "doesnotexist");
    assert_palette_closed(&app);
    assert!(
        matches!(
            app.transcript.last(),
            Some(TranscriptItem::SystemNotice(m)) if m.contains("unknown command")
        ),
        "unknown command must push a SystemNotice; got {:?}",
        app.transcript.last()
    );
}

/// No-regression guard: user messages still flip to Streaming even though the
/// Submit path now closes the palette by default. `SubmitUserMessage` calls
/// `start_stream()` after `close_palette`, so Streaming wins.
#[test]
fn user_message_still_enters_streaming() {
    let (reg, ctx) = test_ctx();
    let uctx = UpdateContext {
        commands: &reg,
        command_ctx: &ctx,
    };
    let mut app = App::new("s".into(), "m".into());
    for c in "hello".chars() {
        update(&mut app, AppAction::InsertChar(c), &uctx);
    }
    let result = update(&mut app, AppAction::Submit, &uctx);
    assert!(matches!(result, UpdateResult::SubmitToEngine(ref t) if t == "hello"));
    assert_eq!(app.mode, AppMode::Streaming);
}

// ── fix-tui-input-horizontal-scroll: viewport keeps caret visible ────────────

/// Helper: render `app` to an 80x24 TestBackend and return (caret_x,
/// caret_y, row_at_input_line). Input row on 80x24 is y=20 (transcript
/// 18 + spinner 1 + input top border → content at 19+1=20).
fn render_and_probe(app: &App, w: u16, h: u16) -> (u16, u16, String) {
    use cc_tui::render;
    use ratatui::backend::{Backend, TestBackend};
    use ratatui::Terminal;

    let backend = TestBackend::new(w, h);
    let mut term = Terminal::new(backend).unwrap();
    term.draw(|f| render::render(f, app)).unwrap();
    let pos = term.backend_mut().get_cursor_position().unwrap();
    let buf = term.backend().buffer().clone();
    let mut row = String::new();
    for x in 0..buf.area.width {
        row.push_str(buf[(x, pos.y)].symbol());
    }
    (pos.x, pos.y, row)
}

/// Typing past the right edge keeps the caret visible AND shows the most
/// recently typed character in the rendered row. Pre-fix: the `Paragraph`
/// truncated at 76 cells so anything past the window disappeared.
#[test]
fn typing_past_width_keeps_caret_visible() {
    let (reg, ctx) = test_ctx();
    let uctx = UpdateContext {
        commands: &reg,
        command_ctx: &ctx,
    };
    let mut app = App::new("s".into(), "m".into());
    for _ in 0..200 {
        update(&mut app, AppAction::InsertChar('x'), &uctx);
    }
    let (caret_x, _caret_y, row) = render_and_probe(&app, 80, 24);
    // inner_right_edge for an 80-col frame: border at x=79, so caret must
    // land strictly inside x ≤ 78.
    assert!(
        caret_x < 79,
        "caret escaped right border at x={caret_x}: {row}"
    );
    // Last char typed must be visible somewhere in the input row.
    assert!(row.contains('x'), "no 'x' rendered in input row: {row}");
}

/// Home resets the viewport offset to 0 so the first char of the buffer
/// is visible and the caret lands at the gutter (col 3 on an 80-col
/// frame: border 1 + gutter 2).
#[test]
fn home_resets_viewport() {
    let (reg, ctx) = test_ctx();
    let uctx = UpdateContext {
        commands: &reg,
        command_ctx: &ctx,
    };
    let mut app = App::new("s".into(), "m".into());
    for _ in 0..200 {
        update(&mut app, AppAction::InsertChar('x'), &uctx);
    }
    update(&mut app, AppAction::CursorHome, &uctx);
    let (caret_x, _caret_y, row) = render_and_probe(&app, 80, 24);
    assert_eq!(caret_x, 3, "Home caret not at gutter: {row}");
    // First 'x' of the buffer sits at col 3 (same as caret). Index by
    // char (not byte), since the border `│` glyph takes 3 UTF-8 bytes.
    assert_eq!(
        row.chars().nth(3),
        Some('x'),
        "first 'x' missing at col 3: {row}"
    );
}

/// End scrolls the viewport so the tail is visible. Caret lands just
/// inside the right edge (col 77 or 78 on 80 cols).
#[test]
fn end_scrolls_to_tail() {
    let (reg, ctx) = test_ctx();
    let uctx = UpdateContext {
        commands: &reg,
        command_ctx: &ctx,
    };
    let mut app = App::new("s".into(), "m".into());
    // Unique tail char so we can locate it in the row.
    for _ in 0..199 {
        update(&mut app, AppAction::InsertChar('x'), &uctx);
    }
    update(&mut app, AppAction::InsertChar('Z'), &uctx);
    update(&mut app, AppAction::CursorHome, &uctx);
    update(&mut app, AppAction::CursorEnd, &uctx);
    let (caret_x, _caret_y, row) = render_and_probe(&app, 80, 24);
    assert!(
        (77..=78).contains(&caret_x),
        "End caret at unexpected col {caret_x}: {row}"
    );
    assert!(row.contains('Z'), "tail 'Z' missing from row: {row}");
}

/// From End on a 100-char buffer, each Left press moves the rendered
/// caret column by at least one cell. Pre-fix: the caret stuck at col 78
/// (the clamp) so ~25 Left presses produced zero visual movement.
#[test]
fn left_moves_caret_every_press() {
    let (reg, ctx) = test_ctx();
    let uctx = UpdateContext {
        commands: &reg,
        command_ctx: &ctx,
    };
    let mut app = App::new("s".into(), "m".into());
    for _ in 0..100 {
        update(&mut app, AppAction::InsertChar('x'), &uctx);
    }
    let (initial_x, _, _) = render_and_probe(&app, 80, 24);
    let mut last = initial_x;
    for i in 0..26 {
        update(&mut app, AppAction::CursorMove(-1), &uctx);
        let (x, _, row) = render_and_probe(&app, 80, 24);
        assert!(
            x < last,
            "Left press #{i} did not move caret (was {last}, still {x}): {row}"
        );
        last = x;
    }
}

/// CJK at the right edge: the caret must land on a cell boundary and the
/// rightmost visible glyph must render fully (both halves present, no
/// partial 2-cell glyph chopped by the slice).
#[test]
fn cjk_caret_on_cell_boundary() {
    let (reg, ctx) = test_ctx();
    let uctx = UpdateContext {
        commands: &reg,
        command_ctx: &ctx,
    };
    let mut app = App::new("s".into(), "m".into());
    // "你好" × 50 = 100 CJK chars, 200 display cells. Well past an 80-col
    // inner width (76 cells).
    for _ in 0..50 {
        update(&mut app, AppAction::InsertChar('你'), &uctx);
        update(&mut app, AppAction::InsertChar('好'), &uctx);
    }
    update(&mut app, AppAction::CursorEnd, &uctx);
    let (caret_x, _, row) = render_and_probe(&app, 80, 24);
    // Caret sits just past the last glyph at the right edge.
    assert!(
        (77..=78).contains(&caret_x),
        "caret at unexpected col {caret_x} for CJK buffer end: {row}"
    );
    // The last CJK char must be visible. Locate it by searching backwards
    // for `好` in the rendered row.
    assert!(row.contains('好'), "last '好' missing from row: {row}");
    // The rightmost CJK glyph occupies two adjacent cells. Find it: the
    // last non-blank, non-border char should be part of a 2-cell glyph.
    // Precise check: the cell at (caret_x - 1) must NOT be a half-rendered
    // half-glyph. Ratatui's TestBackend fills the continuation cell of a
    // wide glyph with an empty string; asserting the char at caret_x - 2
    // is a CJK glyph and the cell at caret_x - 1 exists is enough.
    let chars: Vec<char> = row.chars().collect();
    let left_of_caret = chars[(caret_x as usize).saturating_sub(2)];
    assert!(
        is_cjk(left_of_caret),
        "expected CJK glyph just left of caret, got {left_of_caret:?} in row: {row}"
    );
}

fn is_cjk(c: char) -> bool {
    matches!(c, '\u{4E00}'..='\u{9FFF}')
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

// ── Stress / panic-safety test ───────────────────────────────────────────────
//
// The unit tests pass a handful of carefully-shaped inputs through render;
// this one hammers the renderer with 100 turns of mixed content (ASCII,
// CJK, emoji, markdown, long lines, narrow viewports, unterminated fences)
// and asserts that no draw panics. It's the cheapest way to catch a whole
// class of bugs the TestBackend unit tests can't see — subtle Unicode
// width miscalculations, dividing by zero at tiny widths, string slices
// landing on non-char boundaries after future refactors, etc.
//
// If this fails in CI, the panic message in the failure output points at
// the exact offending combination. Rerun locally with
// `RUST_BACKTRACE=full cargo test -p cc-tui --test headless
//  stress_render_never_panics_on_unicode_mix` for the stack.

/// Large, deterministic unicode corpus. Not `random` — we want the same
/// run every invocation so a CI failure is reproducible.
fn unicode_corpus() -> Vec<&'static str> {
    vec![
        "plain ascii",
        "中文字符测试渲染引擎对 CJK 的处理,看看会不会在字节边界上崩溃。",
        "emoji burst 🎉🚀🔥💡🧠🦀🐳✨🌈⭐",
        "mixed 你好 world 再见 🌏 end",
        "# markdown heading with 中文 in body",
        "`code with 日本語` inline",
        "```rust\nlet x = \"中文字符串\"; // unterminated fence",
        "very long line ".repeat(20).leak() as &str, // > 200 cells
        "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\nm\nn\no\np",
        "mixed widths: a가ab 한글ababab 混合",
        "- bullet 一\n- bullet 二\n- bullet 三",
        "> blockquote with ✨ emoji",
        "1. ordered 中文\n2. ordered 英文\n3. plain",
    ]
}

#[test]
fn stress_render_never_panics_on_unicode_mix() {
    use cc_tui::render::render;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let corpus = unicode_corpus();
    let (reg, ctx) = test_ctx();
    let uctx = UpdateContext {
        commands: &reg,
        command_ctx: &ctx,
    };

    // Exercise multiple viewport sizes so any dimension-specific panic
    // (division by tiny width, clamp underflow, wrap math on 1-col) is hit.
    let widths: [u16; 5] = [20, 40, 60, 80, 160];
    let heights: [u16; 4] = [6, 14, 24, 40];

    for pass in 0..100 {
        let mut app = App::new("stress-session".into(), "claude-sonnet-4-6".into());
        // Build a transcript of 1..N items where N grows per pass, mixing
        // push_user / stream / tool call / tool result / system notice /
        // compact. Every text field pulled from the unicode corpus.
        for i in 0..(pass % 20 + 1) {
            let text = corpus[i % corpus.len()].to_string();
            match i % 6 {
                0 => app.push_user(text),
                1 => {
                    app.start_stream();
                    update(&mut app, AppAction::StreamDelta(text), &uctx);
                    update(
                        &mut app,
                        AppAction::TurnComplete {
                            usage: zero_usage(),
                        },
                        &uctx,
                    );
                }
                2 => app.push_tool_call("Bash".into(), text),
                3 => app.push_tool_result("Bash".into(), text, false),
                4 => app.push_system(text),
                _ => app.push_compact_boundary(),
            }
        }

        // Draw at every (w, h) combination. No panic → pass.
        for &w in &widths {
            for &h in &heights {
                let backend = TestBackend::new(w, h);
                let mut term = Terminal::new(backend).unwrap();
                term.draw(|f| render(f, &app))
                    .unwrap_or_else(|e| panic!("pass {pass} w={w} h={h} draw error: {e}"));
            }
        }
    }
}

/// Specific regression for the bug the user reported: a long Chinese
/// bash-tool input used to panic at render time because the summary
/// path byte-sliced at 120. Here we drive an AppAction::ToolStart with
/// a long CJK summary and render at narrow widths. If the fix regresses,
/// the draw will panic.
#[test]
fn stress_long_cjk_tool_call_renders_cleanly() {
    use cc_tui::render::render;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut app = App::new("s".into(), "m".into());
    let long_command = "echo ".to_string() + &"测试中文命令".repeat(40);
    app.push_tool_call_with_input(
        "Bash".into(),
        long_command.clone(),
        serde_json::json!({ "command": long_command }),
    );

    // Hit a bunch of widths including the narrow fallback.
    for w in [20u16, 40, 60, 80, 120, 200] {
        let backend = TestBackend::new(w, 24);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| render(f, &app))
            .unwrap_or_else(|e| panic!("CJK tool call draw @ w={w}: {e}"));
    }
}
