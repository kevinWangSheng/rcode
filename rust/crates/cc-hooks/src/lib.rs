//! cc-hooks — Hook execution engine.
//!
//! Supports 4 hook kinds: command, prompt, http, agent.
//! Per Phase 2 §5: config snapshot, matcher filtering, parallel execution,
//! deduplication, once-per-session, structured JSON output, CLAUDE_ENV_FILE.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// Hook kind — how the hook is executed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum HookKind {
    #[default]
    Command,
    Prompt,
    Http,
    Agent,
}

/// A single hook configuration entry (from settings.json).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HookConfig {
    /// Hook kind; defaults to "command" if not present.
    #[serde(default, rename = "type")]
    pub kind: HookKind,
    /// Shell command to execute (kind=command).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Static prompt text (kind=prompt).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// HTTP endpoint URL (kind=http).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Subagent identifier (kind=agent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Shell to use (default: "bash").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    /// Condition expression (e.g. "Bash(git *)").
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "if")]
    pub if_condition: Option<String>,
    /// Timeout in seconds (default: 600).
    #[serde(default = "default_timeout")]
    pub timeout: u64,
    /// Status message shown during execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_message: Option<String>,
    /// Run at most once per session.
    #[serde(default)]
    pub once: bool,
    /// Run asynchronously (don't block the tool call).
    #[serde(default, rename = "async")]
    pub is_async: bool,
    /// Rewake the conversation after async hook completes.
    #[serde(default)]
    pub async_rewake: bool,
    /// HTTP headers (kind=http).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<HashMap<String, String>>,
    /// Allowed env vars to pass to hook.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_env_vars: Option<Vec<String>>,
    /// Preserve unknown fields.
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

fn default_timeout() -> u64 {
    600
}

/// A matcher group: event + matcher pattern + list of hooks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookMatcherGroup {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    pub hooks: Vec<HookConfig>,
}

/// All hooks for all events, as stored in settings.json.
pub type HooksSettings = HashMap<String, Vec<HookMatcherGroup>>;

/// Data sent to hooks on stdin as JSON.
#[derive(Debug, Clone, Serialize)]
pub struct HookInput {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    pub cwd: String,
    pub hook_event_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_input: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_response: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
}

/// Structured JSON response from a hook (stdout).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct HookJsonResponse {
    #[serde(default, rename = "continue")]
    pub should_continue: Option<bool>,
    pub stop_reason: Option<String>,
    pub decision: Option<String>,
    pub reason: Option<String>,
    pub system_message: Option<String>,
    pub suppress_output: Option<bool>,
    pub hook_specific_output: Option<HookSpecificOutput>,
    #[serde(rename = "async", default)]
    pub is_async: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct HookSpecificOutput {
    pub permission_decision: Option<String>,
    pub permission_decision_reason: Option<String>,
    pub updated_input: Option<Value>,
    pub additional_context: Option<String>,
}

/// Outcome of running a single hook.
#[derive(Debug)]
pub enum HookOutcome {
    /// Hook ran successfully.
    Ok,
    /// Hook requested a blocking stop.
    Block(String),
    /// Hook failed (non-blocking).
    Failed(String),
    /// Structured JSON response from hook stdout.
    Structured(HookJsonResponse),
}

/// Aggregated result of running all hooks for an event.
#[derive(Debug, Default)]
pub struct HookRunResult {
    pub blocked: bool,
    pub block_message: Option<String>,
    pub env_exports: HashMap<String, String>,
    pub failures: Vec<String>,
}

impl HookRunResult {
    fn empty() -> Self {
        Self::default()
    }
}

/// Deduplication key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct HookKey {
    command_or_url: String,
    if_condition: Option<String>,
}

/// Runs hooks registered for events.
pub struct HookRunner {
    /// Frozen snapshot of hook configuration (taken at session start).
    config_snapshot: HooksSettings,
    /// Set of hooks that have already fired (for `once: true`).
    fired_once: Mutex<HashSet<HookKey>>,
    /// HTTP client.
    http: reqwest::Client,
    /// Whether hooks are disabled entirely.
    disabled: bool,
}

impl HookRunner {
    /// Snapshot the config at session start.
    pub fn new(settings: &HooksSettings, http: reqwest::Client) -> Self {
        Self {
            config_snapshot: settings.clone(),
            fired_once: Mutex::new(HashSet::new()),
            http,
            disabled: false,
        }
    }

    pub fn empty() -> Self {
        Self {
            config_snapshot: HooksSettings::new(),
            fired_once: Mutex::new(HashSet::new()),
            http: reqwest::Client::new(),
            disabled: true,
        }
    }

    /// List all hook event names with at least one handler.
    pub fn event_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .config_snapshot
            .iter()
            .filter(|(_, groups)| !groups.is_empty())
            .map(|(k, _)| k.clone())
            .collect();
        names.sort();
        names
    }

    /// Execute all hooks registered for `event`.
    /// Hooks execute in parallel with individual timeouts.
    pub async fn run(
        &self,
        event: &str,
        input: &HookInput,
        cancel: &CancellationToken,
    ) -> HookRunResult {
        if self.disabled {
            return HookRunResult::empty();
        }

        let groups = match self.config_snapshot.get(event) {
            Some(g) if !g.is_empty() => g,
            _ => return HookRunResult::empty(),
        };

        let input_json = match serde_json::to_string(input) {
            Ok(j) => j,
            Err(e) => {
                return HookRunResult {
                    failures: vec![format!("failed to serialize hook input: {e}")],
                    ..Default::default()
                };
            }
        };

        // Collect matching hooks
        let hooks = self.collect_matching_hooks(groups, input);

        // Deduplicate
        let hooks = self.deduplicate(hooks);

        // Filter once-already-fired
        let hooks = self.filter_once(hooks);

        // Setup CLAUDE_ENV_FILE
        let env_file = setup_env_file();

        // Execute all in parallel
        let futures: Vec<_> = hooks
            .into_iter()
            .map(|h| {
                let input_json = input_json.clone();
                let cancel = cancel.clone();
                let http = self.http.clone();
                let env_path = env_file.as_ref().map(|(p, _)| p.clone());
                async move {
                    let timeout = Duration::from_secs(h.timeout);
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => HookOutcome::Failed("cancelled".into()),
                        result = tokio::time::timeout(
                            timeout,
                            execute_one_hook(&h, &input_json, &http, env_path.as_deref()),
                        ) => {
                            match result {
                                Ok(outcome) => outcome,
                                Err(_) => HookOutcome::Failed(
                                    format!("hook timed out after {}s", h.timeout),
                                ),
                            }
                        }
                    }
                }
            })
            .collect();

        let outcomes = futures::future::join_all(futures).await;

        // Read env exports
        let env_exports = env_file
            .as_ref()
            .map(|(p, _)| read_env_exports(p))
            .unwrap_or_default();

        // Aggregate results
        let mut result = HookRunResult {
            env_exports,
            ..Default::default()
        };
        for outcome in outcomes {
            match outcome {
                HookOutcome::Ok => {}
                HookOutcome::Block(msg) => {
                    result.blocked = true;
                    result.block_message = Some(msg);
                }
                HookOutcome::Failed(msg) => {
                    debug!("hook failed (non-blocking): {msg}");
                    result.failures.push(msg);
                }
                HookOutcome::Structured(resp) => {
                    if resp.decision.as_deref() == Some("block") {
                        result.blocked = true;
                        result.block_message = resp.reason.or(resp.stop_reason);
                    }
                }
            }
        }

        result
    }

    fn collect_matching_hooks<'a>(
        &self,
        groups: &'a [HookMatcherGroup],
        input: &HookInput,
    ) -> Vec<&'a HookConfig> {
        let mut collected = Vec::new();
        for group in groups {
            if let Some(matcher) = &group.matcher {
                // Simple glob match against tool_name
                if let Some(tool_name) = &input.tool_name {
                    if !matches_glob(matcher, tool_name) {
                        continue;
                    }
                } else {
                    continue;
                }
            }
            collected.extend(group.hooks.iter());
        }
        collected
    }

    fn deduplicate<'a>(&self, hooks: Vec<&'a HookConfig>) -> Vec<&'a HookConfig> {
        let mut seen = HashSet::new();
        let mut result = Vec::new();
        for h in hooks {
            let key = HookKey {
                command_or_url: h.command.clone().or_else(|| h.url.clone()).unwrap_or_default(),
                if_condition: h.if_condition.clone(),
            };
            if seen.insert(key) {
                result.push(h);
            }
        }
        result
    }

    fn filter_once<'a>(&self, hooks: Vec<&'a HookConfig>) -> Vec<&'a HookConfig> {
        let mut fired = self.fired_once.lock().unwrap();
        hooks
            .into_iter()
            .filter(|h| {
                if h.once {
                    let key = HookKey {
                        command_or_url: h
                            .command
                            .clone()
                            .or_else(|| h.url.clone())
                            .unwrap_or_default(),
                        if_condition: h.if_condition.clone(),
                    };
                    fired.insert(key)
                } else {
                    true
                }
            })
            .collect()
    }
}

/// Execute a single hook.
async fn execute_one_hook(
    hook: &HookConfig,
    input_json: &str,
    http: &reqwest::Client,
    env_file_path: Option<&Path>,
) -> HookOutcome {
    match &hook.kind {
        HookKind::Command => {
            let Some(command) = hook.command.as_deref().filter(|s| !s.is_empty()) else {
                return HookOutcome::Ok; // silently skip
            };
            let shell = hook.shell.as_deref().unwrap_or("bash");
            run_command_hook(command, input_json, shell, env_file_path).await
        }
        HookKind::Prompt => {
            let Some(text) = hook.prompt.as_deref().filter(|s| !s.is_empty()) else {
                warn!("prompt hook missing `prompt` field; skipping");
                return HookOutcome::Ok;
            };
            HookOutcome::Block(text.to_string())
        }
        HookKind::Http => {
            let Some(url) = hook.url.as_deref().filter(|s| !s.is_empty()) else {
                warn!("http hook missing `url` field; skipping");
                return HookOutcome::Ok;
            };
            run_http_hook(http, url, input_json, &hook.headers).await
        }
        HookKind::Agent => {
            let agent_name = hook.agent.as_deref().unwrap_or("unknown");
            debug!("agent hook delegation requested: {agent_name} (no-op)");
            HookOutcome::Ok
        }
    }
}

async fn run_command_hook(
    command: &str,
    input_json: &str,
    shell: &str,
    env_file_path: Option<&Path>,
) -> HookOutcome {
    let mut cmd = Command::new(shell);
    cmd.arg("-c")
        .arg(command)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    if let Some(path) = env_file_path {
        cmd.env("CLAUDE_ENV_FILE", path);
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return HookOutcome::Failed(format!("failed to spawn hook: {e}")),
    };

    if let Some(mut stdin) = child.stdin.take() {
        let data = format!("{input_json}\n");
        let _ = stdin.write_all(data.as_bytes()).await;
    }

    let output = match child.wait_with_output().await {
        Ok(o) => o,
        Err(e) => return HookOutcome::Failed(format!("failed to wait for hook: {e}")),
    };

    let exit_code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();

    // Try to parse structured JSON from stdout
    if let Ok(resp) = serde_json::from_str::<HookJsonResponse>(&stdout) {
        return HookOutcome::Structured(resp);
    }

    if exit_code == 2 {
        HookOutcome::Block(stdout.trim().to_string())
    } else if exit_code != 0 {
        debug!("hook exited {exit_code}: {command}");
        HookOutcome::Failed(format!("exit {exit_code}"))
    } else {
        HookOutcome::Ok
    }
}

async fn run_http_hook(
    http: &reqwest::Client,
    url: &str,
    input_json: &str,
    extra_headers: &Option<HashMap<String, String>>,
) -> HookOutcome {
    let mut request = http
        .post(url)
        .header("content-type", "application/json")
        .body(input_json.to_string());

    if let Some(headers) = extra_headers {
        for (k, v) in headers {
            request = request.header(k, v);
        }
    }

    let resp = match request.send().await {
        Ok(r) => r,
        Err(e) => return HookOutcome::Failed(format!("http hook request: {e}")),
    };

    if !resp.status().is_success() {
        return HookOutcome::Failed(format!("http hook status: {}", resp.status()));
    }

    let body = resp.text().await.unwrap_or_default();
    if body.is_empty() {
        return HookOutcome::Ok;
    }

    // Try structured JSON response
    if let Ok(resp) = serde_json::from_str::<HookJsonResponse>(&body) {
        return HookOutcome::Structured(resp);
    }

    // Legacy: {"block": true, "message": "..."} format
    if let Ok(v) = serde_json::from_str::<Value>(&body) {
        let block = v.get("block").and_then(|b| b.as_bool()).unwrap_or(false);
        if block {
            let msg = v
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("hook blocked")
                .to_string();
            return HookOutcome::Block(msg);
        }
    }

    HookOutcome::Ok
}

/// Simple glob matching (supports * wildcard only).
fn matches_glob(pattern: &str, text: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if pattern.contains('*') {
        let parts: Vec<&str> = pattern.split('*').collect();
        if parts.len() == 2 {
            return text.starts_with(parts[0]) && text.ends_with(parts[1]);
        }
    }
    pattern == text
}

/// Create a temp file for hook env exports and set CLAUDE_ENV_FILE.
fn setup_env_file() -> Option<(PathBuf, tempfile::NamedTempFile)> {
    let temp = tempfile::NamedTempFile::new().ok()?;
    let path = temp.path().to_owned();
    Some((path, temp))
}

/// After hook execution, read env vars from CLAUDE_ENV_FILE.
fn read_env_exports(path: &Path) -> HashMap<String, String> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    content
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_kind_default_is_command() {
        let cfg: HookConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(cfg.kind, HookKind::Command);
    }

    #[test]
    fn parses_prompt_hook() {
        let json = r#"{"type":"prompt","prompt":"You are concise."}"#;
        let cfg: HookConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.kind, HookKind::Prompt);
        assert_eq!(cfg.prompt.as_deref(), Some("You are concise."));
    }

    #[test]
    fn parses_http_hook() {
        let json = r#"{"type":"http","url":"https://example.com/hook","timeout":15}"#;
        let cfg: HookConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.kind, HookKind::Http);
        assert_eq!(cfg.url.as_deref(), Some("https://example.com/hook"));
        assert_eq!(cfg.timeout, 15);
    }

    #[test]
    fn parses_agent_hook() {
        let json = r#"{"type":"agent","agent":"reviewer"}"#;
        let cfg: HookConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.kind, HookKind::Agent);
        assert_eq!(cfg.agent.as_deref(), Some("reviewer"));
    }

    #[test]
    fn unknown_fields_preserved() {
        let json = r#"{"type":"command","command":"echo hi","future_field":42}"#;
        let cfg: HookConfig = serde_json::from_str(json).unwrap();
        assert!(cfg.extra.contains_key("future_field"));
    }

    #[test]
    fn matcher_group_deserialization() {
        let json = r#"{"matcher":"Bash(git *)","hooks":[{"type":"command","command":"echo hi"}]}"#;
        let group: HookMatcherGroup = serde_json::from_str(json).unwrap();
        assert_eq!(group.matcher.as_deref(), Some("Bash(git *)"));
        assert_eq!(group.hooks.len(), 1);
    }

    #[test]
    fn glob_matching() {
        assert!(matches_glob("*", "anything"));
        assert!(matches_glob("Bash", "Bash"));
        assert!(!matches_glob("Bash", "Read"));
        assert!(matches_glob("Bash*", "BashTool"));
        assert!(matches_glob("*Tool", "BashTool"));
    }

    #[test]
    fn env_file_read_parses_key_values() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("envfile");
        std::fs::write(&path, "KEY1=val1\nKEY2=val2\n").unwrap();
        let exports = read_env_exports(&path);
        assert_eq!(exports.get("KEY1").unwrap(), "val1");
        assert_eq!(exports.get("KEY2").unwrap(), "val2");
    }

    #[tokio::test]
    async fn prompt_hook_blocks_with_text() {
        let settings: HooksSettings = serde_json::from_str(r#"{
            "PreToolUse": [{"hooks": [{"type": "prompt", "prompt": "be careful"}]}]
        }"#)
        .unwrap();
        let runner = HookRunner::new(&settings, reqwest::Client::new());
        let input = HookInput {
            session_id: "test".into(),
            transcript_path: None,
            cwd: "/tmp".into(),
            hook_event_name: "PreToolUse".into(),
            tool_name: None,
            tool_input: None,
            tool_use_id: None,
            tool_response: None,
            source: None,
            model: None,
            message: None,
            agent_id: None,
        };
        let cancel = CancellationToken::new();
        let result = runner.run("PreToolUse", &input, &cancel).await;
        assert!(result.blocked);
        assert_eq!(result.block_message.as_deref(), Some("be careful"));
    }

    #[tokio::test]
    async fn command_hook_exit_2_blocks() {
        let settings: HooksSettings = serde_json::from_str(r#"{
            "PreToolUse": [{"hooks": [{"type": "command", "command": "printf 'no go' && exit 2"}]}]
        }"#)
        .unwrap();
        let runner = HookRunner::new(&settings, reqwest::Client::new());
        let input = HookInput {
            session_id: "test".into(),
            transcript_path: None,
            cwd: "/tmp".into(),
            hook_event_name: "PreToolUse".into(),
            tool_name: None,
            tool_input: None,
            tool_use_id: None,
            tool_response: None,
            source: None,
            model: None,
            message: None,
            agent_id: None,
        };
        let cancel = CancellationToken::new();
        let result = runner.run("PreToolUse", &input, &cancel).await;
        assert!(result.blocked);
        assert_eq!(result.block_message.as_deref(), Some("no go"));
    }

    #[tokio::test]
    async fn once_hook_fires_only_once() {
        let settings: HooksSettings = serde_json::from_str(r#"{
            "PreToolUse": [{"hooks": [{"type": "command", "command": "printf 'blocked' && exit 2", "once": true}]}]
        }"#)
        .unwrap();
        let runner = HookRunner::new(&settings, reqwest::Client::new());
        let input = HookInput {
            session_id: "test".into(),
            transcript_path: None,
            cwd: "/tmp".into(),
            hook_event_name: "PreToolUse".into(),
            tool_name: None,
            tool_input: None,
            tool_use_id: None,
            tool_response: None,
            source: None,
            model: None,
            message: None,
            agent_id: None,
        };
        let cancel = CancellationToken::new();

        // First run should block
        let r1 = runner.run("PreToolUse", &input, &cancel).await;
        assert!(r1.blocked);

        // Second run should not fire (once: true)
        let r2 = runner.run("PreToolUse", &input, &cancel).await;
        assert!(!r2.blocked);
    }

    #[tokio::test]
    async fn empty_runner_returns_empty_result() {
        let runner = HookRunner::empty();
        let input = HookInput {
            session_id: "test".into(),
            transcript_path: None,
            cwd: "/tmp".into(),
            hook_event_name: "PreToolUse".into(),
            tool_name: None,
            tool_input: None,
            tool_use_id: None,
            tool_response: None,
            source: None,
            model: None,
            message: None,
            agent_id: None,
        };
        let cancel = CancellationToken::new();
        let result = runner.run("PreToolUse", &input, &cancel).await;
        assert!(!result.blocked);
    }
}
