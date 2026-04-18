//! Task type implementations.
//!
//! Each function runs a specific task type as an async future that can be
//! spawned into the TaskRegistry.

use cc_core::{CcError, CcResult, SubAgentRunner};
use regex::Regex;
use serde_json::json;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, LazyLock, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::TaskOutput;

/// Notification emitted by the stall watchdog when a local_bash task appears
/// to be blocked on an interactive prompt (no output for `threshold` and the
/// tail looks like a prompt pattern).
#[derive(Debug, Clone)]
pub struct StallNotification {
    pub description: String,
    pub tail: String,
}

/// Watchdog configuration passed into `run_local_bash`. When present, the task
/// streams its stdout/stderr so we can detect stalls; when absent, behavior
/// is unchanged (no streaming overhead, single buffered wait).
#[derive(Clone)]
pub struct StallWatchdog {
    pub description: String,
    pub notifier: mpsc::UnboundedSender<StallNotification>,
    pub threshold: Duration,
    pub check_interval: Duration,
    pub tail_bytes: usize,
}

impl StallWatchdog {
    /// Default 45s threshold / 5s tick / 1024 byte tail, matching the TS
    /// contract in tasks/LocalShellTask/LocalShellTask.tsx.
    pub fn new(description: String, notifier: mpsc::UnboundedSender<StallNotification>) -> Self {
        Self {
            description,
            notifier,
            threshold: Duration::from_secs(45),
            check_interval: Duration::from_secs(5),
            tail_bytes: 1024,
        }
    }
}

// Last-line patterns suggesting the command is blocked on keyboard input.
// Ported from src/tasks/LocalShellTask/LocalShellTask.tsx PROMPT_PATTERNS.
static PROMPT_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"(?i)\(y/n\)",
        r"(?i)\[y/n\]",
        r"(?i)\(yes/no\)",
        r"(?i)\b(?:Do you|Would you|Shall I|Are you sure|Ready to)\b.*\? *$",
        r"(?i)Press (any key|Enter)",
        r"(?i)Continue\?",
        r"(?i)Overwrite\?",
    ]
    .iter()
    .map(|p| Regex::new(p).expect("valid regex"))
    .collect()
});

/// Returns true if the last non-empty line of `tail` matches any prompt pattern.
pub fn looks_like_prompt(tail: &str) -> bool {
    let last_line = tail.trim_end().lines().next_back().unwrap_or("");
    PROMPT_PATTERNS.iter().any(|r| r.is_match(last_line))
}

/// Run a shell command in the background (local_bash task type).
///
/// When `watchdog` is `Some`, stdout/stderr are streamed so a background tick
/// can detect stalls (>=`threshold` with no output) and fire a one-shot
/// notification if the tail matches a prompt pattern. When `None`, behavior
/// is the simple buffered-output path.
pub async fn run_local_bash(
    command: String,
    cwd: PathBuf,
    cancel: CancellationToken,
    watchdog: Option<StallWatchdog>,
) -> CcResult<TaskOutput> {
    let mut child = Command::new("bash")
        .arg("-c")
        .arg(&command)
        .current_dir(&cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| CcError::tool("local_bash", format!("failed to spawn: {e}")))?;

    // Command was built with `Stdio::piped()` for both streams, so `.take()`
    // returns `Some` in practice — but surface an explicit tool error
    // rather than `.expect()`-panicking on the executor thread if that ever
    // changes (e.g., a refactor to `Stdio::null()` for a silent variant).
    let stdout = child.stdout.take().ok_or_else(|| {
        CcError::tool(
            "local_bash",
            "child stdout was not piped; cannot capture output",
        )
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        CcError::tool(
            "local_bash",
            "child stderr was not piped; cannot capture output",
        )
    })?;

    let buf = Arc::new(StdMutex::new(Vec::<u8>::new()));
    let last_activity = Arc::new(StdMutex::new(Instant::now()));

    let stdout_task = spawn_reader(stdout, buf.clone(), last_activity.clone());
    let stderr_task = spawn_reader(stderr, buf.clone(), last_activity.clone());

    let watchdog_task = watchdog.map(|w| {
        let buf = buf.clone();
        let last = last_activity.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move { run_watchdog(w, buf, last, cancel).await })
    });

    let exit_status = tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            let _ = child.kill().await;
            let _ = stdout_task.await;
            let _ = stderr_task.await;
            if let Some(h) = watchdog_task { h.abort(); }
            return Err(CcError::Cancelled);
        }
        status = child.wait() => {
            status.map_err(|e| CcError::tool("local_bash", e.to_string()))?
        }
    };

    // Drain readers so we capture final output before returning.
    let _ = stdout_task.await;
    let _ = stderr_task.await;
    if let Some(h) = watchdog_task {
        h.abort();
    }

    let content = {
        let b = buf.lock().unwrap();
        String::from_utf8_lossy(&b).to_string()
    };
    let exit_code = exit_status.code().unwrap_or(-1);
    Ok(TaskOutput {
        summary: format!("exit {exit_code}"),
        content,
    })
}

fn spawn_reader<R>(
    reader: R,
    buf: Arc<StdMutex<Vec<u8>>>,
    last_activity: Arc<StdMutex<Instant>>,
) -> tokio::task::JoinHandle<()>
where
    R: tokio::io::AsyncRead + Send + Unpin + 'static,
{
    tokio::spawn(async move {
        let mut r = BufReader::new(reader);
        let mut tmp = [0u8; 4096];
        loop {
            match r.read(&mut tmp).await {
                Ok(0) => break,
                Ok(n) => {
                    buf.lock().unwrap().extend_from_slice(&tmp[..n]);
                    *last_activity.lock().unwrap() = Instant::now();
                }
                Err(_) => break,
            }
        }
    })
}

async fn run_watchdog(
    w: StallWatchdog,
    buf: Arc<StdMutex<Vec<u8>>>,
    last_activity: Arc<StdMutex<Instant>>,
    cancel: CancellationToken,
) {
    let mut ticker = tokio::time::interval(w.check_interval);
    ticker.tick().await;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = ticker.tick() => {
                let elapsed = last_activity.lock().unwrap().elapsed();
                if elapsed < w.threshold {
                    continue;
                }
                let tail = {
                    let b = buf.lock().unwrap();
                    let start = b.len().saturating_sub(w.tail_bytes);
                    String::from_utf8_lossy(&b[start..]).to_string()
                };
                if looks_like_prompt(&tail) {
                    let _ = w.notifier.send(StallNotification {
                        description: w.description.clone(),
                        tail,
                    });
                    return; // one-shot
                }
                // Not a prompt — reset so we don't re-tail every tick.
                *last_activity.lock().unwrap() = Instant::now();
            }
        }
    }
}

/// Run an in-process sub-agent (local_agent task type).
///
/// Executes a single agent turn and returns the output. Wired via the
/// `SubAgentRunner` trait to avoid cc-agents → cc-query circular dependency.
pub async fn run_local_agent(
    prompt: String,
    runner: Arc<dyn SubAgentRunner>,
    cancel: CancellationToken,
) -> CcResult<TaskOutput> {
    let content = runner.run(None, prompt, Vec::new(), cancel).await?;
    Ok(TaskOutput {
        summary: "agent completed".into(),
        content,
    })
}

/// Run an in-process teammate agent (in_process_teammate task type).
///
/// Runs an initial turn, then loops processing messages from the mailbox
/// inbox until the inbox is closed or the task is cancelled. Each inbox
/// message becomes a new user turn.
pub async fn run_in_process_teammate(
    prompt: String,
    runner: Arc<dyn SubAgentRunner>,
    mut inbox: mpsc::Receiver<String>,
    cancel: CancellationToken,
) -> CcResult<TaskOutput> {
    // Run initial turn
    let mut last_output = runner.run(None, prompt, Vec::new(), cancel.clone()).await?;

    // Process inbox messages until cancelled or closed
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            msg = inbox.recv() => {
                match msg {
                    None => break, // inbox closed (TeamDelete called)
                    Some(message) => {
                        last_output = runner
                            .run(None, message, Vec::new(), cancel.clone())
                            .await?;
                    }
                }
            }
        }
    }

    Ok(TaskOutput {
        summary: "teammate completed".into(),
        content: last_output,
    })
}

/// Remote agent task type: delegate to a remote Claude Code instance via HTTP API.
pub async fn run_remote_agent(
    prompt: String,
    endpoint: String,
    http: reqwest::Client,
    cancel: CancellationToken,
) -> CcResult<TaskOutput> {
    let body = json!({
        "prompt": prompt,
    });

    tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            Err(CcError::Cancelled)
        }
        result = http.post(&endpoint)
            .header("content-type", "application/json")
            .json(&body)
            .send() => {
            let response = result
                .map_err(|e| CcError::Other(format!("remote agent request failed: {e}")))?;

            if !response.status().is_success() {
                return Err(CcError::Other(format!(
                    "remote agent returned status {}",
                    response.status()
                )));
            }

            let text = response.text().await
                .map_err(|e| CcError::Other(format!("remote agent response read failed: {e}")))?;

            Ok(TaskOutput {
                summary: "remote agent completed".into(),
                content: text,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_bash_runs_command() {
        let cancel = CancellationToken::new();
        let result = run_local_bash(
            "echo hello".into(),
            std::env::current_dir().unwrap(),
            cancel,
            None,
        )
        .await
        .unwrap();
        assert_eq!(result.summary, "exit 0");
        assert!(result.content.contains("hello"));
    }

    #[tokio::test]
    async fn local_bash_captures_exit_code() {
        let cancel = CancellationToken::new();
        let result = run_local_bash(
            "exit 42".into(),
            std::env::current_dir().unwrap(),
            cancel,
            None,
        )
        .await
        .unwrap();
        assert_eq!(result.summary, "exit 42");
    }

    #[tokio::test]
    async fn local_bash_cancellation() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = run_local_bash(
            "sleep 60".into(),
            std::env::current_dir().unwrap(),
            cancel,
            None,
        )
        .await;
        assert!(matches!(result, Err(CcError::Cancelled)));
    }

    #[test]
    fn prompt_pattern_matches() {
        assert!(looks_like_prompt("Proceed? (y/n)"));
        assert!(looks_like_prompt("[y/N]"));
        assert!(looks_like_prompt("Overwrite?"));
        assert!(looks_like_prompt("Press any key to continue"));
        assert!(looks_like_prompt("Are you sure you want to delete?"));
        assert!(looks_like_prompt(
            "some earlier output\nDo you want to continue?"
        ));
    }

    #[test]
    fn prompt_pattern_rejects_non_prompts() {
        assert!(!looks_like_prompt("Building project..."));
        assert!(!looks_like_prompt("error: something broke"));
        assert!(!looks_like_prompt(""));
        assert!(!looks_like_prompt(
            "compiling module X\ndone compiling module X"
        ));
    }

    #[tokio::test]
    async fn watchdog_fires_on_prompt_stall() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut wd = StallWatchdog::new("test cmd".into(), tx);
        wd.threshold = Duration::from_millis(150);
        wd.check_interval = Duration::from_millis(50);
        let cancel = CancellationToken::new();
        // Print a prompt then sleep longer than the threshold.
        let result = run_local_bash(
            "echo 'Overwrite?' && sleep 1".into(),
            std::env::current_dir().unwrap(),
            cancel,
            Some(wd),
        )
        .await
        .unwrap();
        assert_eq!(result.summary, "exit 0");
        let notif = rx.try_recv().expect("watchdog should have fired");
        assert_eq!(notif.description, "test cmd");
        assert!(notif.tail.contains("Overwrite?"));
    }

    #[tokio::test]
    async fn watchdog_silent_on_non_prompt_stall() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut wd = StallWatchdog::new("test cmd".into(), tx);
        wd.threshold = Duration::from_millis(150);
        wd.check_interval = Duration::from_millis(50);
        let cancel = CancellationToken::new();
        let result = run_local_bash(
            "echo 'Building...' && sleep 1".into(),
            std::env::current_dir().unwrap(),
            cancel,
            Some(wd),
        )
        .await
        .unwrap();
        assert_eq!(result.summary, "exit 0");
        assert!(
            rx.try_recv().is_err(),
            "watchdog should not fire on non-prompt tail"
        );
    }

    #[tokio::test]
    async fn watchdog_silent_on_active_output() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut wd = StallWatchdog::new("test cmd".into(), tx);
        wd.threshold = Duration::from_millis(300);
        wd.check_interval = Duration::from_millis(50);
        let cancel = CancellationToken::new();
        // Keep producing output faster than the threshold.
        let result = run_local_bash(
            "for i in 1 2 3 4 5 6 7 8; do echo line $i; sleep 0.1; done".into(),
            std::env::current_dir().unwrap(),
            cancel,
            Some(wd),
        )
        .await
        .unwrap();
        assert_eq!(result.summary, "exit 0");
        assert!(
            rx.try_recv().is_err(),
            "watchdog should not fire while output is growing"
        );
    }
}
