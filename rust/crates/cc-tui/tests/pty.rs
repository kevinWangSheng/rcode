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
}

impl Default for LaunchOpts {
    fn default() -> Self {
        LaunchOpts {
            cols: 100,
            rows: 24,
            script: "[]".to_string(),
            log_path: None,
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
