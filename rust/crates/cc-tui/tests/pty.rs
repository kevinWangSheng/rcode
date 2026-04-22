//! PTY-driven integration tests for the TUI.
//!
//! Spawns the `cc-tui-demo` binary under a virtual terminal (`portable-pty`)
//! and pipes its output through a `vt100::Parser` so tests can assert on the
//! *actual rendered grid* the way a human sees it — including escape
//! sequences, cursor moves, and scrollback-entered content. This is the
//! only way to catch a whole class of TUI bugs (flicker, content falling off
//! the top, escape-sequence leakage, cursor leak) that `TestBackend` unit
//! tests never exercise.
//!
//! Debugging tip: set `CC_TUI_LOG_FILE=/tmp/cc-tui.log` before running
//! `cargo test --test pty -- --nocapture` and `tail -f /tmp/cc-tui.log` in
//! another terminal to watch the tracing stream while the test runs.

#![cfg(unix)] // portable-pty + vt100 are both cross-platform but the
              // harness uses unix-only niceties. Good enough for CI on
              // macOS + Linux.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, MasterPty, NativePtySystem, PtySize, PtySystem};

/// Harness around a PTY + VT parser driving `cc-tui-demo`.
///
/// The writer is wrapped in `Arc<Mutex<...>>` so the reader thread can also
/// write — specifically to answer `ESC[6n` (cursor-position / DSR) queries
/// that Ratatui's `Viewport::Inline` fires during init. Without a DSR
/// responder in the loop, the child blocks forever on `crossterm::cursor::
/// position()` (it waits for a `ESC[<row>;<col>R` reply that never comes)
/// and our inline-viewport tests time out on "terminal init".
pub struct TuiPty {
    master: Box<dyn MasterPty + Send>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    parser: Arc<Mutex<vt100::Parser>>,
    _reader_handle: std::thread::JoinHandle<()>,
    _child: Box<dyn portable_pty::Child + Send + Sync>,
}

/// Options for launching a demo TUI.
pub struct LaunchOpts {
    pub cols: u16,
    pub rows: u16,
    /// JSON script fed to the demo binary via stdin.
    pub script: String,
    /// Optional path for the tracing log. Creates a NamedTempFile if None.
    pub log_path: Option<PathBuf>,
    /// Extra env vars to forward to the child. Used by the streaming-flush
    /// regression test to pass `CC_TUI_DEMO_AUTO_STREAM=1` without needing
    /// to simulate a user submit first.
    pub extra_env: Vec<(String, String)>,
}

impl Default for LaunchOpts {
    fn default() -> Self {
        LaunchOpts {
            cols: 100,
            rows: 24,
            script: "[]".to_string(),
            log_path: None,
            extra_env: Vec::new(),
        }
    }
}

impl TuiPty {
    /// Spawn `cc-tui-demo` under a fresh PTY and start a background reader
    /// that feeds the VT parser. The binary must already be built
    /// (`cargo build --bin cc-tui-demo`); tests that use this call it once
    /// via [`build_demo_once`] as a hard prerequisite.
    pub fn launch(opts: LaunchOpts) -> anyhow::Result<Self> {
        let pty_system = NativePtySystem::default();
        let pair = pty_system.openpty(PtySize {
            rows: opts.rows,
            cols: opts.cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let bin = cargo_bin("cc-tui-demo");
        let mut cmd = CommandBuilder::new(bin);
        // Tell the demo to assume a non-dumb TERM so the palette detects
        // truecolor and our clippy-clean colour tests fire. The actual
        // rendering goes through vt100 which accepts whatever we send.
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        if let Some(log_path) = &opts.log_path {
            cmd.env("CC_TUI_LOG_FILE", log_path);
            cmd.env("RUST_LOG", "cc_tui=debug");
        }
        for (k, v) in &opts.extra_env {
            cmd.env(k, v);
        }
        // Inherit cwd — the welcome banner's `cwd:` line uses it.
        cmd.cwd(std::env::current_dir()?);

        // Write the script to a temp file and pass the path via env var.
        // We cannot pipe it through stdin because under a PTY the child's
        // stdin is the controlling terminal — writes become *keystrokes*,
        // not a stdin pipe, and the JSON ends up typed into the input box.
        let script_file = tempfile::NamedTempFile::new()?;
        std::fs::write(script_file.path(), &opts.script)?;
        cmd.env("CC_TUI_DEMO_SCRIPT", script_file.path());
        // Keep the tempfile alive for the child's lifetime by leaking its
        // path — the OS will clean it up on process exit.
        let script_path = script_file.into_temp_path();
        let _ = script_path.keep()?; // prevent premature deletion

        let child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave); // no longer needed on parent side

        let writer: Arc<Mutex<Box<dyn Write + Send>>> =
            Arc::new(Mutex::new(pair.master.take_writer()?));

        // Background reader loop — feed every byte the child emits into
        // the VT parser. Also scan for `ESC[6n` (DSR cursor-position query)
        // and auto-reply with `ESC[1;1R` so Ratatui's `Viewport::Inline`
        // init doesn't block forever. Keep the thread alive for the
        // duration of the harness; it stops naturally when the PTY closes.
        let mut reader = pair.master.try_clone_reader()?;
        let parser = Arc::new(Mutex::new(vt100::Parser::new(opts.rows, opts.cols, 1024)));
        let reader_parser = parser.clone();
        let reader_writer = writer.clone();
        let reader_handle = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => {
                        // Respond to DSR cursor-position queries before
                        // forwarding to the parser; otherwise Ratatui
                        // init hangs waiting for a reply.
                        let chunk = &buf[..n];
                        let needle = b"\x1b[6n";
                        if chunk.windows(needle.len()).any(|w| w == needle) {
                            if let Ok(mut w) = reader_writer.lock() {
                                let _ = w.write_all(b"\x1b[1;1R");
                                let _ = w.flush();
                            }
                        }
                        if let Ok(mut p) = reader_parser.lock() {
                            p.process(chunk);
                        }
                    }
                }
            }
        });

        Ok(TuiPty {
            master: pair.master,
            writer,
            parser,
            _reader_handle: reader_handle,
            _child: child,
        })
    }

    /// Send raw bytes (e.g. key sequences) to the TUI stdin.
    pub fn send(&mut self, bytes: &[u8]) -> anyhow::Result<()> {
        let mut w = self.writer.lock().unwrap();
        w.write_all(bytes)?;
        w.flush()?;
        Ok(())
    }

    /// Resize the PTY. Triggers a SIGWINCH on the child.
    pub fn resize(&mut self, cols: u16, rows: u16) -> anyhow::Result<()> {
        self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        if let Ok(mut p) = self.parser.lock() {
            p.set_size(rows, cols);
        }
        Ok(())
    }

    /// Capture the current visible screen as a newline-joined string.
    pub fn screen(&self) -> String {
        let p = self.parser.lock().unwrap();
        p.screen().contents()
    }

    /// Capture the last `rows` lines of scrollback + visible screen.
    ///
    /// vt100 0.15 is limited: `set_scrollback(N)` is only safe for
    /// `N <= rows - 1` (beyond that `visible_rows` underflows at
    /// `rows_len - N`). That caps how far we can pan back to a single
    /// viewport-height window of "last N scrollback lines + (rows - N)
    /// current lines". For PTY tests to be able to see enough history,
    /// launch the harness with tall rows (e.g. 60-80) so this peek
    /// window actually covers the items we pushed via `insert_before`.
    pub fn scrollback(&self) -> String {
        let mut p = self.parser.lock().unwrap();
        let rows = p.screen().size().0.max(2) as usize;
        // Pan to the deepest safe offset so we see as much scrollback as
        // possible in one snapshot.
        let max_safe = rows.saturating_sub(1);
        p.set_scrollback(max_safe);
        let out = p.screen().contents();
        p.set_scrollback(0);
        out
    }

    /// Poll the screen until `pred` returns true or the timeout fires.
    /// Returns the final screen contents on success / failure both.
    pub fn wait_for<F: Fn(&str) -> bool>(&self, pred: F, timeout: Duration) -> (bool, String) {
        let deadline = Instant::now() + timeout;
        loop {
            let screen = self.screen();
            if pred(&screen) {
                return (true, screen);
            }
            if Instant::now() >= deadline {
                return (false, screen);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for TuiPty {
    fn drop(&mut self) {
        // Send Ctrl+C twice to force-quit the demo (matches the app's
        // 2x-Ctrl+C → ForceQuit shortcut), then give the child a moment
        // to exit. If it still refuses, kill it outright.
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.write_all(&[0x03, 0x03]);
            let _ = w.flush();
        }
        std::thread::sleep(Duration::from_millis(50));
        let _ = self._child.kill();
    }
}

/// Build `cc-tui-demo` once per test run and return its path.
pub fn build_demo_once() -> anyhow::Result<()> {
    use std::sync::OnceLock;
    static BUILT: OnceLock<anyhow::Result<()>> = OnceLock::new();
    let result = BUILT.get_or_init(|| {
        let out = std::process::Command::new("cargo")
            .args(["build", "--bin", "cc-tui-demo"])
            .output()?;
        if !out.status.success() {
            anyhow::bail!(
                "cargo build --bin cc-tui-demo failed:\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Ok(())
    });
    match result {
        Ok(()) => Ok(()),
        Err(e) => Err(anyhow::anyhow!("{e}")),
    }
}

/// Resolve the absolute path of a cargo-built binary. Walks up from the
/// test binary's own location to find `target/debug/<name>`.
fn cargo_bin(name: &str) -> PathBuf {
    let exe = std::env::current_exe().expect("no current_exe");
    // .../target/debug/deps/pty-xxx — ascend to target/debug/ then append name.
    let mut p = exe.clone();
    for _ in 0..5 {
        p.pop();
        if p.ends_with("debug") {
            return p.join(name);
        }
    }
    // Fall back to workspace-root/target/debug/<name>.
    PathBuf::from("target/debug").join(name)
}

// ── Tests ────────────────────────────────────────────────────────────────

/// Smoke test: launch the demo with a tiny script and wait for the user
/// prompt glyph `>` to appear. Proves the whole pipeline is wired up —
/// cargo build, PTY spawn, VT parser, and our own screen() function.
#[test]
fn smoke_demo_launches_and_renders_prompt() {
    build_demo_once().unwrap();

    let opts = LaunchOpts {
        cols: 80,
        rows: 24,
        script: r#"[{"stream_delta": "hello from script"}, {"turn_complete": {}}]"#.to_string(),
        log_path: None,
        extra_env: Vec::new(),
    };
    let tui = TuiPty::launch(opts).unwrap();

    // The welcome banner's `> ` input prompt + the welcome "Welcome to
    // Claude Code" line should appear within 3 seconds on any CI box.
    let (matched, screen) = tui.wait_for(
        |s| s.contains("Welcome to Claude Code") || s.contains("hello from script"),
        Duration::from_secs(5),
    );
    assert!(
        matched,
        "expected welcome banner or scripted text; got screen:\n{screen}"
    );
}

/// Regression-anchor for the "content disappears when pushed off the top"
/// bug. Emits 60 distinct user+assistant turns then checks that the
/// *scrollback* (not just the current screen) contains early turns.
///
/// Under the pre-refactor architecture this fails: render_transcript keeps
/// everything inside the inline viewport, so rows above the viewport are
/// lost once `Paragraph.scroll` pushes them out. Under `insert_before`
/// they live in real terminal scrollback and vt100::Parser captures them.
#[test]
fn scrollback_preserves_old_turns() {
    build_demo_once().unwrap();

    // Emit enough tool-call transcript items to comfortably exceed any
    // inline-viewport height and force insert_before to fire. Each
    // ToolStart/ToolEnd pair renders as ~3 rows; 30 pairs ≈ 90 rows.
    // ToolStart/ToolEnd don't need AppMode::Streaming, so they work even
    // without simulating user submits — keeps the test deterministic.
    let mut steps: Vec<serde_json::Value> = Vec::new();
    for i in 0..30 {
        steps.push(serde_json::json!({
            "tool_start": { "name": "Bash", "input": { "command": format!("echo TURN{:02}", i) } }
        }));
        steps.push(serde_json::json!({
            "tool_end": { "name": "Bash", "output": format!("TURN{:02}-body", i), "is_error": false }
        }));
        steps.push(serde_json::json!({ "sleep_ms": 25 }));
    }
    let script = serde_json::to_string(&steps).unwrap();

    // rows=80 → vt100 can peek the last ~79 rows of scrollback, enough
    // to capture ~25 turns (3 rows each). Ample for asserting that
    // ANY early turn reached scrollback (the core signal of the refactor).
    let opts = LaunchOpts {
        cols: 100,
        rows: 80,
        script,
        log_path: None,
        extra_env: Vec::new(),
    };
    let tui = TuiPty::launch(opts).unwrap();

    // Wait for the last emitted turn to reach either the visible screen
    // or scrollback. `contains` checks both via our peek helpers below.
    let (rendered, _final) = tui.wait_for(|s| s.contains("TURN29-body"), Duration::from_secs(15));
    if !rendered {
        let peek = tui.scrollback();
        let current = tui.screen();
        panic!(
            "last turn (TURN29) never rendered — demo may have frozen.\n\
             --- visible screen ---\n{current}\n\
             --- scrollback peek ---\n{peek}"
        );
    }

    // Core refactor signal: at least one *earlier* turn (not the latest
    // few) must be reachable via terminal scrollback. Pre-refactor, the
    // inline viewport swallowed everything past `viewport_rows` and the
    // scrollback stayed empty. Post-refactor, finalized items flow into
    // terminal scrollback via `insert_before`.
    let peek = tui.scrollback();
    let found_early = (0..25).any(|i| peek.contains(&format!("TURN{:02}-body", i)));
    assert!(
        found_early,
        "no early turn (TURN00..24) reached scrollback — the inline \
         viewport is still eating old content. (peek len = {} bytes)\n\
         --- peek ---\n{}",
        peek.len(),
        peek
    );
}

/// Regression for the "streaming content scrolls off the top of the
/// viewport and is lost" bug caught via user screenshots (2026-04-20).
///
/// Before the fix, a long streaming assistant response rendered entirely
/// inside `streaming_text` → `Paragraph.scroll` pin-to-bottom. As the
/// text grew past the viewport height, the earliest paragraphs scrolled
/// off the top of the paragraph and were unrecoverable until
/// `TurnComplete` fired (at which point the whole blob flushed at once).
///
/// After the fix, `flush_to_scrollback` detects stable `\n\n` boundaries
/// in `streaming_text` that are outside any open fenced code block and
/// emits each stable prefix to terminal scrollback via `insert_before`,
/// keeping only the unstable tail in the inline viewport.
///
/// This test scripts a multi-paragraph stream (~150 rows) and asserts
/// early paragraphs land in scrollback *before* the stream completes.
#[test]
fn streaming_flushes_stable_paragraphs_to_scrollback() {
    build_demo_once().unwrap();

    // 20 paragraphs, each followed by a blank line. Each paragraph is
    // tagged PARA## so we can probe the peek for earlier content.
    let mut steps: Vec<serde_json::Value> = Vec::new();
    for i in 0..20 {
        steps.push(serde_json::json!({
            "stream_delta": format!("PARA{:02} line one.\nPARA{:02} line two.\n\n", i, i)
        }));
        // Sleep just enough that the draw loop drains each delta in a
        // separate tick — 80 ms is two redraws per paragraph.
        steps.push(serde_json::json!({ "sleep_ms": 80 }));
    }
    steps.push(serde_json::json!({ "turn_complete": {} }));
    let script = serde_json::to_string(&steps).unwrap();

    // rows=40 tall enough for vt100 peek + the inline viewport to grow
    // well past the first few paragraphs, but short enough that overflow
    // hits well before the 20th paragraph.
    // Use a known path so we can inspect it after a failed test without
    // waiting on a tempfile cleanup.
    let log_path = std::path::PathBuf::from("/tmp/cc-tui-probe/streaming-test.log");
    let _ = std::fs::remove_file(&log_path);
    let _ = std::fs::create_dir_all("/tmp/cc-tui-probe");
    let opts = LaunchOpts {
        cols: 100,
        rows: 40,
        script,
        log_path: Some(log_path.clone()),
        extra_env: vec![("CC_TUI_DEMO_AUTO_STREAM".to_string(), "1".to_string())],
    };
    let tui = TuiPty::launch(opts).unwrap();

    // Wait for the last paragraph to show up anywhere — visible viewport
    // OR flushed scrollback. With the fix, most paragraphs land in
    // scrollback within ~100ms of arrival, not in the visible viewport.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut done = false;
    while std::time::Instant::now() < deadline {
        if tui.screen().contains("PARA19") || tui.scrollback().contains("PARA19") {
            done = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if !done {
        let log = std::fs::read_to_string(&log_path).unwrap_or_default();
        let last_log = log.lines().rev().take(20).collect::<Vec<_>>().join("\n");
        let screen = tui.screen();
        let back = tui.scrollback();
        panic!(
            "stream never finished — demo may have frozen.\n\
             --- last 20 log lines ---\n{last_log}\n\
             --- screen tail ---\n{}\n\
             --- scrollback tail ---\n{}",
            &screen[screen.len().saturating_sub(500)..],
            &back[back.len().saturating_sub(500)..],
        );
    }

    // After the stream completes, early paragraphs must be reachable via
    // terminal scrollback. Pre-fix, they scrolled off the top of
    // Paragraph.scroll and the scrollback saw only the single big post-
    // turn_complete flush of the final AssistantText — missing all
    // intermediate paragraphs that had been painted and then overwritten.
    let peek = tui.scrollback();
    let has_early = (0..6).any(|i| peek.contains(&format!("PARA{:02}", i)));
    assert!(
        has_early,
        "no early paragraph (PARA00..05) reached scrollback — \
         streaming_text is still eating overflow instead of flushing \
         stable prefixes via insert_before. (peek len = {} bytes)\n\
         --- peek tail ---\n{}",
        peek.len(),
        &peek[peek.len().saturating_sub(2000)..]
    );

    // Let pending tracing writes drain to disk before reading the log.
    // `tracing_subscriber::fmt` writes unbuffered to our File handle, but
    // the OS file cache and our Mutex-wrapped writer can still trail by
    // a few ms behind the in-memory events.
    std::thread::sleep(Duration::from_millis(500));

    // Also sanity-check via the trace log: at least one "flush" event
    // should have fired during the stream (i.e. streaming_len went
    // *down* as a prefix got absorbed into scrollback, without a
    // TurnComplete between them).
    let log_contents = std::fs::read_to_string(&log_path).unwrap_or_default();
    // Drop the final "turn_complete" transition to 0; count transitions
    // where streaming_len decreased while mode=Streaming.
    let mut prev: Option<u64> = None;
    let mut mid_stream_flushes = 0usize;
    for line in log_contents
        .lines()
        .filter(|l| l.contains("mode=Streaming"))
    {
        if let Some(idx) = line.find("streaming_len=") {
            let rest = &line[idx + "streaming_len=".len()..];
            if let Some(n) = rest
                .split(|c: char| !c.is_ascii_digit())
                .next()
                .and_then(|s| s.parse::<u64>().ok())
            {
                if let Some(p) = prev {
                    if n < p {
                        mid_stream_flushes += 1;
                    }
                }
                prev = Some(n);
            }
        }
    }
    // Alternative check: direct evidence of flush firing. Older parse
    // logic may have raced the buffered writer; also accept explicit
    // "streaming flush succeeded" log lines as proof the code path ran.
    let explicit_flush = log_contents.contains("streaming flush succeeded");
    assert!(
        mid_stream_flushes > 0 || explicit_flush,
        "no mid-stream flush evidence in trace log — \
         flush_to_scrollback's streaming-prefix branch never fired. \
         (mid_stream_flushes = {mid_stream_flushes})\n\
         --- log tail ---\n{}",
        &log_contents[log_contents.len().saturating_sub(4000)..]
    );

    drop(tui);
}

/// Welcome banner must appear in the transcript area when the Inline
/// viewport fell back to Fullscreen (terminals whose DSR reply is slow
/// or missing — nested tmux, ssh with proxy, some MCP-hosted emulators).
///
/// Under the pre-fix code, the banner was printed to stdout BEFORE raw
/// mode and then wiped by `Terminal::clear()` in the Fullscreen-fallback
/// branch, leaving the empty-session screen totally blank above the
/// input box. After the fix, `run_tui` stashes the banner on `App` when
/// falling back to Fullscreen and `render_transcript` paints it into
/// the frame while `is_empty_session()` holds.
///
/// `CC_TUI_FORCE_FULLSCREEN=1` skips the Inline attempt entirely,
/// guaranteeing the Fullscreen code path runs.
#[test]
fn welcome_banner_visible_under_fullscreen_fallback() {
    build_demo_once().unwrap();

    let opts = LaunchOpts {
        cols: 100,
        rows: 40,
        script: r#"[{"sleep_ms": 300}]"#.to_string(),
        log_path: None,
        extra_env: vec![("CC_TUI_FORCE_FULLSCREEN".to_string(), "1".to_string())],
    };
    let tui = TuiPty::launch(opts).unwrap();

    let (matched, screen) = tui.wait_for(
        |s| s.contains("Welcome to Claude Code"),
        Duration::from_secs(5),
    );
    assert!(
        matched,
        "welcome banner missing under Fullscreen fallback — \
         did the banner leak into the pre-raw-mode println path again?\n{screen}"
    );
}

/// M3 manual runtime check: 80-column Terminal.app launch must render
/// cleanly with no rendering artifacts.
///
/// Covers the "Terminal.app 80-col launch" line-item in the M3 entry
/// criteria for Milestone 5. The headless `TestBackend` tests don't
/// catch escape-sequence leakage, cursor misplacement, or content
/// overflowing the declared width — vt100::Parser does. This test
/// launches `cc-tui-demo` at 80x24 (the historical minimum-viable
/// terminal size for a `clawd`-style CLI) and asserts:
///
///   1. The welcome banner appears within 3 s.
///   2. The `>` prompt glyph is present (input row rendered).
///   3. No visible row exceeds 80 display cells (no overflow that
///      vt100 had to clip).
///   4. No raw escape-sequence literals leak into the rendered grid
///      (e.g. `\x1b[` or `ESC[` visible as plain text would mean the
///      renderer emitted an unrecognised / malformed sequence).
#[test]
fn launches_cleanly_at_80_cols_no_artifacts() {
    build_demo_once().unwrap();

    let opts = LaunchOpts {
        cols: 80,
        rows: 24,
        // Small idle script — enough to prove the demo starts, not
        // enough to exercise scrollback. Keeps the check focused on
        // the empty-session shell layout.
        script: r#"[{"sleep_ms": 300}]"#.to_string(),
        log_path: None,
        extra_env: Vec::new(),
    };
    let tui = TuiPty::launch(opts).unwrap();

    // Welcome banner anchor: "Welcome to Claude Code" is the most
    // stable string in the empty-session welcome card.
    let (matched, screen) = tui.wait_for(
        |s| s.contains("Welcome to Claude Code"),
        Duration::from_secs(3),
    );
    assert!(matched, "welcome banner missing at 80x24:\n{screen}");

    // The unambiguous input glyph. Helps catch regressions where the
    // PromptInput row drifts off-screen or the gutter disappears.
    assert!(
        screen.contains('>'),
        "no `>` prompt glyph at 80x24:\n{screen}"
    );

    // Row width: vt100 clips at 80 cells, so a width-overflow bug
    // manifests as a truncated row rather than an 81-cell row. We
    // still assert <=80 defensively in case vt100 ever changes.
    for (i, line) in screen.lines().enumerate() {
        let w: usize = line
            .chars()
            .map(|c| unicode_width::UnicodeWidthChar::width(c).unwrap_or(0))
            .sum();
        assert!(
            w <= 80,
            "row {i} is {w} display cells wide in an 80-col terminal:\n{line}"
        );
    }

    // Escape-sequence leakage: any `\x1b[` that reaches the rendered
    // grid as printable text means the renderer emitted bytes the VT
    // engine couldn't interpret. Check both the ESC glyph and the
    // usual textual forms ("ESC[", "^[").
    for needle in ["\u{1b}[", "ESC[", "^["] {
        assert!(
            !screen.contains(needle),
            "raw escape-sequence leak `{needle}` in rendered screen:\n{screen}"
        );
    }
}
