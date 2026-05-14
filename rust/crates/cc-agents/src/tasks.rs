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

/// Metadata persisted for a remote-agent task across resumes.
/// Mirrors TS `RemoteAgentMetadata` in `services/agents/teleport.ts`:
/// the session id lets the next session reconnect to a running
/// remote turn instead of starting a fresh one.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RemoteAgentMetadata {
    pub session_id: String,
    pub endpoint: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
}

/// Callback invoked once the remote server hands back a session id
/// — wired by the engine to persist the metadata in cc-session so
/// `--resume` can reconnect to the running turn.
pub type RemoteAgentMetadataSink = Box<dyn Fn(&RemoteAgentMetadata) + Send + Sync + 'static>;

/// Poll interval for `run_remote_agent`. Matches TS
/// `REMOTE_AGENT_POLL_INTERVAL_MS = 1500`.
const REMOTE_AGENT_POLL_MS: u64 = 1500;
/// Overall ceiling. After this, the task errors with a timeout so
/// the leader can decide whether to restart or surface the failure.
/// Matches TS `REMOTE_AGENT_TIMEOUT_MS = 10 * 60_000`.
const REMOTE_AGENT_TIMEOUT_MS: u64 = 10 * 60_000;

/// Remote agent task type: delegate to a remote Claude Code instance.
///
/// Two-phase protocol:
///   1. `POST {endpoint}/sessions` with `{prompt}` → response carries
///      `{session_id, status}` and optionally `{content}` if the
///      server completed synchronously (small jobs).
///   2. Otherwise enter a `GET {endpoint}/sessions/{session_id}` poll
///      loop. Server replies with `{status, content?}` where status
///      ∈ `{queued, running, completed, failed}`. Loop exits on a
///      terminal status, the overall timeout, or cancel.
///
/// If the server responds with a plain-text body (legacy / sync-only
/// servers), the function preserves the pre-change behaviour: treat
/// the body as the final content and return immediately. New servers
/// gain async polling without breaking old ones.
///
/// `metadata_sink` (optional) is invoked once after the create
/// response is parsed so cc-session can persist
/// [`RemoteAgentMetadata`] for resume.
pub async fn run_remote_agent(
    prompt: String,
    endpoint: String,
    http: reqwest::Client,
    cancel: CancellationToken,
    metadata_sink: Option<RemoteAgentMetadataSink>,
) -> CcResult<TaskOutput> {
    let body = json!({ "prompt": prompt });
    let create_url = format!("{}/sessions", endpoint.trim_end_matches('/'));

    let resp = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(CcError::Cancelled),
        result = http.post(&create_url)
            .header("content-type", "application/json")
            .json(&body)
            .send() => {
            result.map_err(|e| CcError::Other(format!("remote agent request failed: {e}")))?
        }
    };

    if !resp.status().is_success() {
        return Err(CcError::Other(format!(
            "remote agent returned status {}",
            resp.status()
        )));
    }

    // Prefer JSON when the server advertises it; fall back to the
    // legacy plain-text path so older servers still work.
    let ctype = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("text/plain")
        .to_ascii_lowercase();

    let body_text = resp
        .text()
        .await
        .map_err(|e| CcError::Other(format!("remote agent response read failed: {e}")))?;

    if !ctype.contains("json") {
        // Legacy one-shot server: body IS the answer.
        return Ok(TaskOutput {
            summary: "remote agent completed".into(),
            content: body_text,
        });
    }

    let parsed: serde_json::Value = serde_json::from_str(&body_text).map_err(|e| {
        CcError::Other(format!(
            "remote agent returned malformed JSON: {e}; body was {body_text}"
        ))
    })?;

    let session_id = parsed
        .get("session_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| CcError::Other("remote agent response missing session_id".into()))?
        .to_string();

    if let Some(sink) = metadata_sink.as_ref() {
        sink(&RemoteAgentMetadata {
            session_id: session_id.clone(),
            endpoint: endpoint.clone(),
            started_at: chrono::Utc::now(),
        });
    }

    // Synchronous completion path.
    let status = parsed
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("running");
    if is_terminal_status(status) {
        return finalise(&parsed, status);
    }

    // Poll loop.
    let poll_url = format!("{}/sessions/{session_id}", endpoint.trim_end_matches('/'));
    let started = std::time::Instant::now();
    let interval = std::time::Duration::from_millis(REMOTE_AGENT_POLL_MS);
    let timeout = std::time::Duration::from_millis(REMOTE_AGENT_TIMEOUT_MS);

    loop {
        if started.elapsed() > timeout {
            return Err(CcError::Other(format!(
                "remote agent timed out after {}s (session {session_id})",
                timeout.as_secs()
            )));
        }

        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(CcError::Cancelled),
            _ = tokio::time::sleep(interval) => {}
        }

        let poll_resp = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(CcError::Cancelled),
            result = http.get(&poll_url).send() => result,
        }
        .map_err(|e| CcError::Other(format!("remote agent poll failed: {e}")))?;

        if !poll_resp.status().is_success() {
            // Transient failure: warn and retry until the timeout
            // fires. A 5xx that lingers eventually surfaces via the
            // ceiling above; 4xx (session gone) should be terminal.
            if poll_resp.status().is_client_error() {
                return Err(CcError::Other(format!(
                    "remote agent poll returned {}; session may have been evicted",
                    poll_resp.status()
                )));
            }
            tracing::warn!(
                "remote agent poll {} returned {}; retrying",
                poll_url,
                poll_resp.status()
            );
            continue;
        }

        let raw = poll_resp
            .text()
            .await
            .map_err(|e| CcError::Other(format!("remote agent poll read failed: {e}")))?;
        let snapshot: serde_json::Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("remote agent poll body unparseable: {e}; raw: {raw}");
                continue;
            }
        };
        let status = snapshot
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("running");
        if is_terminal_status(status) {
            return finalise(&snapshot, status);
        }
    }
}

fn is_terminal_status(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled" | "error")
}

fn finalise(snapshot: &serde_json::Value, status: &str) -> CcResult<TaskOutput> {
    let content = snapshot
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if status == "completed" {
        Ok(TaskOutput {
            summary: "remote agent completed".into(),
            content,
        })
    } else {
        let error_msg = snapshot
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("remote agent ended in non-success state");
        Err(CcError::Other(format!(
            "remote agent status={status}: {error_msg}"
        )))
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

    /// P0 #18: a server that returns a synchronous `{session_id,
    /// status:"completed", content}` response short-circuits the
    /// poll loop and surfaces the content immediately. Also
    /// verifies the metadata sink fires exactly once with the
    /// session id the server handed back.
    #[tokio::test]
    async fn remote_agent_handles_synchronous_completion() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());

        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await.unwrap();
            let body = r#"{"session_id":"rs-sync-1","status":"completed","content":"hello world"}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
        });

        let captured: Arc<Mutex<Vec<RemoteAgentMetadata>>> = Arc::new(Mutex::new(Vec::new()));
        let captured_clone = captured.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = calls.clone();
        let sink: RemoteAgentMetadataSink = Box::new(move |meta| {
            calls_clone.fetch_add(1, Ordering::SeqCst);
            captured_clone.lock().unwrap().push(meta.clone());
        });

        let result = run_remote_agent(
            "hi".into(),
            endpoint.clone(),
            reqwest::Client::new(),
            CancellationToken::new(),
            Some(sink),
        )
        .await
        .unwrap();

        assert_eq!(result.summary, "remote agent completed");
        assert_eq!(result.content, "hello world");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let snapshot: Vec<RemoteAgentMetadata> = captured.lock().unwrap().clone();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].session_id, "rs-sync-1");
        assert_eq!(snapshot[0].endpoint, endpoint);
        server.await.unwrap();
    }

    /// P0 #18: when the create response returns `status:"running"`,
    /// the task polls `/sessions/{id}` until a terminal status
    /// shows up. The metadata sink still fires exactly once at
    /// session creation (not on every poll).
    #[tokio::test]
    async fn remote_agent_polls_until_terminal_status() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());

        let server = tokio::spawn(async move {
            // 1. POST /sessions → 200 { session_id, status: running }
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await.unwrap();
            let body = r#"{"session_id":"rs-poll-2","status":"running"}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;

            // 2. GET /sessions/rs-poll-2 → still running
            let (mut sock, _) = listener.accept().await.unwrap();
            let _ = sock.read(&mut buf).await.unwrap();
            let body = r#"{"status":"running"}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;

            // 3. GET /sessions/rs-poll-2 → completed with content
            let (mut sock, _) = listener.accept().await.unwrap();
            let _ = sock.read(&mut buf).await.unwrap();
            let body = r#"{"status":"completed","content":"final answer"}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
        });

        // Tight poll interval for the test so we don't wait 1.5s
        // between polls. We swap via a #[cfg(test)]-gated path,
        // but the const is module-private — easier to just live
        // with the default. Drop to a CancellationToken with a
        // short timeout so flake budget stays sane.
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = calls.clone();
        let sink: RemoteAgentMetadataSink = Box::new(move |_meta| {
            calls_clone.fetch_add(1, Ordering::SeqCst);
        });

        let result = run_remote_agent(
            "compute something".into(),
            endpoint.clone(),
            reqwest::Client::new(),
            CancellationToken::new(),
            Some(sink),
        )
        .await
        .unwrap();

        assert_eq!(result.summary, "remote agent completed");
        assert_eq!(result.content, "final answer");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "metadata sink must fire exactly once per task"
        );
        server.await.unwrap();
    }

    /// P0 #18: cancel during the poll wait short-circuits with
    /// `CcError::Cancelled` even if the server has been answering
    /// running on every poll.
    #[tokio::test]
    async fn remote_agent_polling_honors_cancel() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());

        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await.unwrap();
            let body = r#"{"session_id":"rs-cancel","status":"running"}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
            // We accept further polls but never reply quickly — the
            // cancel should fire during the inter-poll sleep before
            // any poll completes.
            let _ = listener.accept().await;
        });

        let cancel = CancellationToken::new();
        let cancel_inner = cancel.clone();
        // Fire cancel after the create response has been parsed and
        // the function is sleeping inside the poll loop.
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            cancel_inner.cancel();
        });
        let err = run_remote_agent("x".into(), endpoint, reqwest::Client::new(), cancel, None)
            .await
            .unwrap_err();
        assert!(matches!(err, CcError::Cancelled), "got: {err}");
        server.abort();
    }
}
