//! `cc-tui-demo` — drives `cc_tui::run_tui` with a scripted sequence of
//! events instead of a real `QueryEngine`, so the whole TUI event loop can
//! be exercised under a PTY harness without an API key or network.
//!
//! Script format (JSON, one step per array element):
//!
//! ```json
//! [
//!   { "sleep_ms": 100 },
//!   { "stream_delta": "hello " },
//!   { "stream_delta": "world" },
//!   { "tool_start": { "name": "Bash", "input": { "command": "ls" } } },
//!   { "tool_end":   { "name": "Bash", "output": "a\nb", "is_error": false } },
//!   { "turn_complete": {} },
//!   { "compact": {} },
//!   { "error": "something bad" }
//! ]
//! ```
//!
//! The script is read from `$CC_TUI_DEMO_SCRIPT` (file path) or `stdin`.
//! Size can be overridden via `CC_TUI_DEMO_SIZE=80x24` for PTY harnesses
//! that want deterministic geometry before launch.
//!
//! Example manual invocation:
//!
//! ```bash
//! echo '[{"stream_delta":"hi"},{"turn_complete":{}}]' \
//!   | cargo run --bin cc-tui-demo
//! ```

use std::io::Read;
use std::time::Duration;

use cc_core::AppEvent as CoreEvent;
use cc_tui::{CommandContext, CommandRegistry, TuiConfig};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Step {
    /// Pause N milliseconds before the next step.
    SleepMs(u64),
    /// Emit a streaming text delta.
    StreamDelta(String),
    /// Start a tool call with the given name + raw JSON input.
    ToolStart {
        name: String,
        input: serde_json::Value,
    },
    /// End a tool call with output text.
    ToolEnd {
        name: String,
        output: String,
        #[serde(default)]
        is_error: bool,
    },
    /// Finish the current streaming turn.
    TurnComplete {},
    /// Mark a compaction boundary.
    Compact {},
    /// Inject a system error message.
    Error(String),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Read script from file path or stdin. Stdin is the common path for PTY
    // harnesses that pipe the script in.
    let script_json = match std::env::var_os("CC_TUI_DEMO_SCRIPT") {
        Some(path) => std::fs::read_to_string(&path)?,
        None => {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s)?;
            s
        }
    };
    let script: Vec<Step> = serde_json::from_str(&script_json)?;

    let (events_tx, events_rx) = mpsc::channel::<CoreEvent>(512);

    // Spawn the scripted task. It holds a Sender so the main loop's
    // events_rx sees EOF only when we drop it, which happens naturally
    // when the task ends and the receiver (in run_tui) continues with the
    // select!.
    let tx = events_tx.clone();
    tokio::spawn(async move {
        for step in script {
            match step {
                Step::SleepMs(ms) => tokio::time::sleep(Duration::from_millis(ms)).await,
                Step::StreamDelta(text) => {
                    let _ = tx.send(CoreEvent::StreamDelta(text)).await;
                }
                Step::ToolStart { name, input } => {
                    let _ = tx.send(CoreEvent::ToolStart { name, input }).await;
                }
                Step::ToolEnd {
                    name,
                    output,
                    is_error,
                } => {
                    let _ = tx
                        .send(CoreEvent::ToolEnd {
                            name,
                            result: cc_core::ToolResult {
                                content: output,
                                is_error,
                            },
                        })
                        .await;
                }
                Step::TurnComplete {} => {
                    let _ = tx
                        .send(CoreEvent::TurnComplete {
                            usage: cc_core::Usage::default(),
                        })
                        .await;
                }
                Step::Compact {} => {
                    let _ = tx.send(CoreEvent::CompactBoundary).await;
                }
                Step::Error(msg) => {
                    let _ = tx.send(CoreEvent::Error(msg)).await;
                }
            }
        }
        // After the script ends, hold the channel open for a grace window so
        // the TUI keeps running and the harness can still send keystrokes.
        // 60s is generous for any CI-speed test + plenty of room to
        // interactively explore under a PTY.
        tokio::time::sleep(Duration::from_secs(60)).await;
        drop(tx);
    });

    let cfg = TuiConfig {
        model: "demo-model".to_string(),
        session_id: "demo-session".to_string(),
        engine: None,
        messages: Vec::new(),
        cancel: CancellationToken::new(),
        commands: CommandRegistry::empty(),
        command_ctx: CommandContext::new(env!("CARGO_PKG_VERSION"), "demo-model"),
        events_tx,
        events_rx,
        version: env!("CARGO_PKG_VERSION").to_string(),
        cwd: Some("~/demo".to_string()),
        git_branch: Some("demo-branch".to_string()),
    };

    cc_tui::run_tui(cfg).await?;
    Ok(())
}
