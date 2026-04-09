use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tracing::{debug, warn};

/// A configured hook entry. The `type` field selects the variant; if absent we
/// default to `command` for backward-compat with the old M2 schema where the
/// only kind of hook was a shell command.
///
/// Supported types:
///   - `command` — shell command, JSON input on stdin (existing M2 behavior).
///   - `prompt` — appends `text` to the next user turn. Returns Block(text)
///     on PreToolUse so the engine can surface it.
///   - `http` — POST the hook input to a URL. Non-2xx → Failed (non-block).
///     A 2xx with `{"block": true, "message": "..."}` body blocks.
///   - `agent` — placeholder: logs a delegation request and returns Ok. The
///     full subagent runner lives outside this milestone; this type exists so
///     settings.json that references agents loads cleanly without "unknown
///     field" errors.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HookConfig {
    /// Hook kind; defaults to "command" if not present.
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    /// Shell command to execute (kind=command).
    #[serde(default)]
    pub command: Option<String>,
    /// Static prompt text (kind=prompt).
    #[serde(default)]
    pub text: Option<String>,
    /// HTTP endpoint URL (kind=http).
    #[serde(default)]
    pub url: Option<String>,
    /// Subagent identifier (kind=agent).
    #[serde(default)]
    pub agent: Option<String>,
    /// Timeout in seconds (default: 600).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
    /// Absorb unknown fields (e.g. TS plugin hooks with `hooks`, `matcher`, etc.).
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, Value>,
}

impl HookConfig {
    /// Return true only if this entry has an executable command.
    pub fn has_command(&self) -> bool {
        self.command.as_deref().map(|s| !s.is_empty()).unwrap_or(false)
    }

    /// Resolve the kind, defaulting to "command" when absent.
    pub fn resolved_kind(&self) -> &str {
        self.kind.as_deref().unwrap_or("command")
    }
}

/// Settings hooks section — keyed by event name.
/// e.g. `{ "PreToolUse": [{ "command": "my-hook.sh" }] }`
pub type HooksConfig = std::collections::HashMap<String, Vec<HookConfig>>;

/// Outcome of running a hook.
#[derive(Debug)]
pub enum HookOutcome {
    /// Hook ran successfully (exit 0).
    Ok,
    /// Hook requested a blocking stop (exit 2). Contains the message to surface.
    Block(String),
    /// Hook failed (non-zero exit other than 2) — non-blocking.
    Failed(String),
}

/// Input sent to hooks via stdin as JSON.
#[derive(Debug, Serialize)]
pub struct HookInput<'a> {
    pub event: &'a str,
    pub tool_name: &'a str,
    pub tool_input: &'a Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<&'a str>,
}

/// Runs hooks registered for a given event.
#[derive(Debug, Clone)]
pub struct HookRunner {
    config: HooksConfig,
}

impl HookRunner {
    pub fn new(config: HooksConfig) -> Self {
        HookRunner { config }
    }

    pub fn empty() -> Self {
        HookRunner {
            config: HooksConfig::new(),
        }
    }

    /// List all hook event names that have at least one configured handler.
    /// Used by the TUI to render `/hooks`. Sorted for stable display.
    pub fn event_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .config
            .iter()
            .filter(|(_, hooks)| !hooks.is_empty())
            .map(|(k, _)| k.clone())
            .collect();
        names.sort();
        names
    }

    /// Execute all hooks registered for `event`.
    /// Returns the first blocking outcome if any hook exits with code 2.
    pub async fn run(&self, event: &str, input: &HookInput<'_>) -> HookOutcome {
        let hooks = match self.config.get(event) {
            Some(h) if !h.is_empty() => h,
            _ => return HookOutcome::Ok,
        };

        let input_json = match serde_json::to_string(input) {
            Ok(j) => j,
            Err(e) => {
                return HookOutcome::Failed(format!("failed to serialize hook input: {e}"));
            }
        };

        for hook in hooks {
            let timeout_secs = hook.timeout.unwrap_or(600);
            let kind = hook.resolved_kind();
            debug!("running hook (kind={kind}, event={event})");

            let outcome = match kind {
                "command" => {
                    let Some(command) = hook
                        .command
                        .as_deref()
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                    else {
                        // TS-format / unrelated entry — silently skip.
                        continue;
                    };
                    run_command_hook(&command, &input_json, timeout_secs).await
                }
                "prompt" => {
                    let Some(text) = hook.text.as_deref().filter(|s| !s.is_empty()) else {
                        warn!("prompt hook missing `text` field; skipping");
                        continue;
                    };
                    HookOutcome::Block(text.to_string())
                }
                "http" => {
                    let Some(url) = hook.url.as_deref().filter(|s| !s.is_empty()) else {
                        warn!("http hook missing `url` field; skipping");
                        continue;
                    };
                    run_http_hook(url, &input_json, timeout_secs).await
                }
                "agent" => {
                    let Some(agent_name) = hook.agent.as_deref().filter(|s| !s.is_empty()) else {
                        warn!("agent hook missing `agent` field; skipping");
                        continue;
                    };
                    debug!("agent hook delegation requested: {agent_name} (no-op runner in M4)");
                    HookOutcome::Ok
                }
                other => {
                    debug!("unknown hook kind '{other}' — skipping");
                    continue;
                }
            };

            // Stop on the first blocking outcome; otherwise loop on.
            if let HookOutcome::Block(_) = &outcome {
                return outcome;
            }
            if let HookOutcome::Failed(e) = &outcome {
                debug!("hook failed (non-blocking): {e}");
            }
        }

        HookOutcome::Ok
    }
}

async fn run_command_hook(command: &str, input_json: &str, timeout_secs: u64) -> HookOutcome {
    let result = tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        run_hook_command(command, input_json),
    )
    .await;

    match result {
        Err(_) => HookOutcome::Failed(format!("hook timed out after {timeout_secs}s: {command}")),
        Ok(Err(e)) => HookOutcome::Failed(format!("hook error: {e}")),
        Ok(Ok((exit_code, stdout, _stderr))) => {
            if exit_code == 2 {
                HookOutcome::Block(stdout.trim().to_string())
            } else {
                if exit_code != 0 {
                    debug!("hook exited {exit_code}: {command}");
                }
                HookOutcome::Ok
            }
        }
    }
}

async fn run_http_hook(url: &str, input_json: &str, timeout_secs: u64) -> HookOutcome {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .build()
    {
        Ok(c) => c,
        Err(e) => return HookOutcome::Failed(format!("http client build: {e}")),
    };

    let resp = match client
        .post(url)
        .header("content-type", "application/json")
        .body(input_json.to_string())
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return HookOutcome::Failed(format!("http hook request: {e}")),
    };

    if !resp.status().is_success() {
        return HookOutcome::Failed(format!("http hook status: {}", resp.status()));
    }

    // Body convention: `{"block": true, "message": "..."}` → blocking; anything
    // else → ok. Empty body / non-json body → ok.
    let body = resp.text().await.unwrap_or_default();
    if body.is_empty() {
        return HookOutcome::Ok;
    }
    match serde_json::from_str::<Value>(&body) {
        Ok(v) => {
            let block = v.get("block").and_then(|b| b.as_bool()).unwrap_or(false);
            if block {
                let msg = v
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("hook blocked")
                    .to_string();
                HookOutcome::Block(msg)
            } else {
                HookOutcome::Ok
            }
        }
        Err(_) => HookOutcome::Ok,
    }
}

async fn run_hook_command(
    command: &str,
    stdin_data: &str,
) -> Result<(i32, String, String), String> {
    let mut child = Command::new("bash")
        .arg("-c")
        .arg(command)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn hook: {e}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        let data = format!("{stdin_data}\n");
        stdin
            .write_all(data.as_bytes())
            .await
            .map_err(|e| format!("failed to write hook stdin: {e}"))?;
    }

    let output = child
        .wait_with_output()
        .await
        .map_err(|e| format!("failed to wait for hook: {e}"))?;

    let exit_code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    Ok((exit_code, stdout, stderr))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ts_format_hook_has_no_command() {
        // TS-format entry: { "hooks": [...], "async": true } — no top-level "command"
        let json = r#"{"hooks":[{"async":true,"command":"node notify.js","type":"command"}]}"#;
        let cfg: HookConfig = serde_json::from_str(json).unwrap();
        assert!(!cfg.has_command(), "TS-format hook should have no top-level command");
    }

    #[test]
    fn direct_command_hook_is_detected() {
        let json = r#"{"command":"my-hook.sh","timeout":30}"#;
        let cfg: HookConfig = serde_json::from_str(json).unwrap();
        assert!(cfg.has_command());
        assert_eq!(cfg.command.as_deref(), Some("my-hook.sh"));
        assert_eq!(cfg.timeout, Some(30));
    }

    #[test]
    fn hooks_config_mixed_entries() {
        // Simulate the real ~/.claude/settings.json hooks section
        let json = r#"{
            "PreToolUse": [
                {"hooks":[{"async":true,"command":"node notify.js","type":"command"}]},
                {"command":"my-pre-hook.sh"}
            ]
        }"#;
        let config: HooksConfig = serde_json::from_str(json).unwrap();
        let pre = config.get("PreToolUse").unwrap();
        assert_eq!(pre.len(), 2);
        assert!(!pre[0].has_command(), "TS-format entry should be skipped");
        assert!(pre[1].has_command(), "direct command entry should be kept");
    }

    #[test]
    fn parses_prompt_hook_kind() {
        let json = r#"{"type":"prompt","text":"You are concise."}"#;
        let cfg: HookConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.resolved_kind(), "prompt");
        assert_eq!(cfg.text.as_deref(), Some("You are concise."));
    }

    #[test]
    fn parses_http_hook_kind() {
        let json = r#"{"type":"http","url":"https://example.com/hook","timeout":15}"#;
        let cfg: HookConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.resolved_kind(), "http");
        assert_eq!(cfg.url.as_deref(), Some("https://example.com/hook"));
        assert_eq!(cfg.timeout, Some(15));
    }

    #[test]
    fn parses_agent_hook_kind() {
        let json = r#"{"type":"agent","agent":"reviewer"}"#;
        let cfg: HookConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.resolved_kind(), "agent");
        assert_eq!(cfg.agent.as_deref(), Some("reviewer"));
    }

    #[test]
    fn untagged_hook_defaults_to_command() {
        let json = r#"{"command":"echo hi"}"#;
        let cfg: HookConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.resolved_kind(), "command");
    }

    #[tokio::test]
    async fn prompt_hook_blocks_with_text() {
        let mut cfg = HooksConfig::new();
        cfg.insert(
            "PreToolUse".into(),
            vec![HookConfig {
                kind: Some("prompt".into()),
                text: Some("be careful".into()),
                ..Default::default()
            }],
        );
        let runner = HookRunner::new(cfg);
        let input = HookInput {
            event: "PreToolUse",
            tool_name: "Bash",
            tool_input: &Value::Null,
            session_id: None,
        };
        match runner.run("PreToolUse", &input).await {
            HookOutcome::Block(msg) => assert_eq!(msg, "be careful"),
            other => panic!("expected Block, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn command_hook_runs_and_blocks_on_exit_2() {
        // Use exit code 2 (block) with stdout payload — runner should return Block.
        let mut cfg = HooksConfig::new();
        cfg.insert(
            "PreToolUse".into(),
            vec![HookConfig {
                kind: Some("command".into()),
                command: Some("printf 'no go' && exit 2".into()),
                ..Default::default()
            }],
        );
        let runner = HookRunner::new(cfg);
        let input = HookInput {
            event: "PreToolUse",
            tool_name: "Bash",
            tool_input: &Value::Null,
            session_id: None,
        };
        match runner.run("PreToolUse", &input).await {
            HookOutcome::Block(msg) => assert_eq!(msg, "no go"),
            other => panic!("expected Block, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn http_hook_blocks_when_server_says_block() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            // Read until we have a full HTTP request (headers + body of declared
            // content-length). This avoids reqwest hanging on a server that closes
            // the connection mid-stream.
            let mut total = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = sock.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                total.extend_from_slice(&buf[..n]);
                if let Some(pos) = total.windows(4).position(|w| w == b"\r\n\r\n") {
                    let header_str = std::str::from_utf8(&total[..pos]).unwrap_or("");
                    let cl: usize = header_str
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    let body_start = pos + 4;
                    if total.len() - body_start >= cl {
                        break;
                    }
                }
            }
            let body = r#"{"block":true,"message":"nope"}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
        });

        let mut cfg = HooksConfig::new();
        cfg.insert(
            "PreToolUse".into(),
            vec![HookConfig {
                kind: Some("http".into()),
                url: Some(format!("http://{addr}/hook")),
                timeout: Some(5),
                ..Default::default()
            }],
        );
        let runner = HookRunner::new(cfg);
        let input = HookInput {
            event: "PreToolUse",
            tool_name: "Bash",
            tool_input: &Value::Null,
            session_id: None,
        };
        match runner.run("PreToolUse", &input).await {
            HookOutcome::Block(msg) => assert_eq!(msg, "nope"),
            other => panic!("expected Block from http hook, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn agent_hook_is_currently_a_noop() {
        let mut cfg = HooksConfig::new();
        cfg.insert(
            "PreToolUse".into(),
            vec![HookConfig {
                kind: Some("agent".into()),
                agent: Some("reviewer".into()),
                ..Default::default()
            }],
        );
        let runner = HookRunner::new(cfg);
        let input = HookInput {
            event: "PreToolUse",
            tool_name: "Bash",
            tool_input: &Value::Null,
            session_id: None,
        };
        assert!(matches!(
            runner.run("PreToolUse", &input).await,
            HookOutcome::Ok
        ));
    }
}
