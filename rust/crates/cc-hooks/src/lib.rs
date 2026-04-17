//! cc-hooks — Hook execution engine.
//!
//! Supports 4 hook kinds: command, prompt, http, agent.
//! Per Phase 2 §5: config snapshot, matcher filtering, parallel execution,
//! deduplication, once-per-session, structured JSON output, CLAUDE_ENV_FILE.
//!
//! Types (HookConfig, HookInput, etc.) are defined in cc_core::hook.
//! This crate provides the runtime execution engine.

// Re-export cc_core hook types for convenience of downstream crates.
pub use cc_core::hook::{
    HookConfig, HookInput, HookJsonResponse, HookKind, HookMatcherGroup, HookOutcome,
    HooksSettings,
};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// Aggregated result of running all hooks for an event.
#[derive(Debug, Default)]
pub struct HookRunResult {
    pub blocked: bool,
    pub block_message: Option<String>,
    pub env_exports: HashMap<String, String>,
    pub failures: Vec<String>,
    /// Extra context strings returned by hooks via `hook_specific_output.additional_context`.
    /// Used by SessionStart / SubagentStart / UserPromptSubmit to inject context
    /// as a user message before the next turn.
    pub additional_contexts: Vec<String>,
}

impl HookRunResult {
    fn empty() -> Self {
        Self::default()
    }
}

/// Deduplication key (§5.2: command_or_url, if_condition, namespace).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct HookKey {
    command_or_url: String,
    if_condition: Option<String>,
    namespace: Option<String>,
}

/// Runs hooks registered for events.
pub struct HookRunner {
    /// Frozen snapshot of hook configuration (taken at session start).
    config_snapshot: HooksSettings,
    /// Session-scoped hooks (added at runtime, e.g. by function hooks).
    session_hooks: RwLock<HooksSettings>,
    /// Set of hooks that have already fired (for `once: true`).
    fired_once: Mutex<HashSet<HookKey>>,
    /// HTTP client.
    http: reqwest::Client,
    /// If true, only managed (policy-path) hooks are allowed.
    managed_only: bool,
    /// Whether hooks are disabled entirely.
    disabled: bool,
}

impl HookRunner {
    /// Snapshot the config at session start (prevents race with settings changes).
    pub fn new(settings: &HooksSettings, http: reqwest::Client) -> Self {
        Self {
            config_snapshot: settings.clone(),
            session_hooks: RwLock::new(HooksSettings::new()),
            fired_once: Mutex::new(HashSet::new()),
            http,
            managed_only: false,
            disabled: false,
        }
    }

    pub fn empty() -> Self {
        Self {
            config_snapshot: HooksSettings::new(),
            session_hooks: RwLock::new(HooksSettings::new()),
            fired_once: Mutex::new(HashSet::new()),
            http: reqwest::Client::new(),
            managed_only: false,
            disabled: true,
        }
    }

    /// Set managed-only mode (only policy-path hooks are allowed).
    pub fn set_managed_only(&mut self, managed_only: bool) {
        self.managed_only = managed_only;
    }

    /// Add a session-scoped hook at runtime (e.g. from function hooks).
    pub fn add_session_hook(&self, event: &str, group: HookMatcherGroup) {
        let mut hooks = self.session_hooks.write().unwrap();
        hooks.entry(event.to_string()).or_default().push(group);
    }

    /// Run `SessionEnd` hooks with a tight 1.5s timeout (§5.2 behavior contract).
    /// Respects `CLAUDE_CODE_SESSIONEND_HOOKS_TIMEOUT_MS` env var override.
    pub async fn run_session_end(&self, input: &HookInput) {
        if self.disabled {
            return;
        }
        let timeout_ms: u64 = std::env::var("CLAUDE_CODE_SESSIONEND_HOOKS_TIMEOUT_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1500);
        let cancel = CancellationToken::new();
        let result_fut = self.run("SessionEnd", input, &cancel);
        let _ = tokio::time::timeout(Duration::from_millis(timeout_ms), result_fut).await;
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

        let snapshot_groups = self.config_snapshot.get(event);

        // Clone session-scoped hooks out of the RwLock before borrowing
        let session_groups_owned: Vec<HookMatcherGroup> = self
            .session_hooks
            .read()
            .ok()
            .and_then(|s| s.get(event).cloned())
            .unwrap_or_default();

        let has_snapshot = snapshot_groups.is_some_and(|g| !g.is_empty());
        if !has_snapshot && session_groups_owned.is_empty() {
            return HookRunResult::empty();
        }

        let input_json = match serde_json::to_string(input) {
            Ok(j) => j,
            Err(e) => {
                return HookRunResult {
                    failures: vec![format!("failed to serialize hook input: {e}")],
                    ..Default::default()
                };
            }
        };

        // Collect matching hooks from snapshot + session
        let empty_groups = Vec::new();
        let groups = snapshot_groups.unwrap_or(&empty_groups);
        let mut hooks = self.collect_matching_hooks(groups, input);
        if !session_groups_owned.is_empty() {
            hooks.extend(self.collect_matching_hooks(&session_groups_owned, input));
        }

        // Deduplicate
        let hooks = self.deduplicate(hooks);

        // Filter once-already-fired
        let hooks = self.filter_once(hooks);

        // Setup CLAUDE_ENV_FILE
        let env_file = setup_env_file();

        // Extract env context from input for subprocess injection
        let session_id = input.session_id.clone();
        let cwd = input.cwd.clone();

        // Execute all in parallel
        let futures: Vec<_> = hooks
            .into_iter()
            .map(|h| {
                let input_json = input_json.clone();
                let cancel = cancel.clone();
                let http = self.http.clone();
                let env_path = env_file.as_ref().map(|(p, _)| p.clone());
                let sid = session_id.clone();
                let cwd = cwd.clone();
                async move {
                    let timeout = Duration::from_secs(h.timeout);
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => (HookOutcome::Failed("cancelled".into()), None),
                        result = tokio::time::timeout(
                            timeout,
                            execute_one_hook(h, &input_json, &http, env_path.as_deref(), &sid, &cwd),
                        ) => {
                            match result {
                                Ok(pair) => pair,
                                Err(_) => (HookOutcome::Failed(
                                    format!("hook timed out after {}s", h.timeout),
                                ), None),
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
        for (outcome, extra_ctx) in outcomes {
            if let Some(ctx) = extra_ctx {
                if !ctx.is_empty() {
                    result.additional_contexts.push(ctx);
                }
            }
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
            let key = hook_key(h);
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
                    fired.insert(hook_key(h))
                } else {
                    true
                }
            })
            .collect()
    }
}

/// Execute a single hook. Returns (outcome, optional additional_context).
async fn execute_one_hook(
    hook: &HookConfig,
    input_json: &str,
    http: &reqwest::Client,
    env_file_path: Option<&Path>,
    session_id: &str,
    cwd: &str,
) -> (HookOutcome, Option<String>) {
    match &hook.kind {
        HookKind::Command => {
            let Some(command) = hook.command.as_deref().filter(|s| !s.is_empty()) else {
                return (HookOutcome::Ok, None); // silently skip
            };
            let shell = hook.shell.as_deref().unwrap_or("bash");
            run_command_hook(command, input_json, shell, env_file_path, session_id, cwd).await
        }
        HookKind::Prompt => {
            // cc-core uses `text` field (with `prompt` as serde alias)
            let Some(text) = hook.text.as_deref().filter(|s| !s.is_empty()) else {
                warn!("prompt hook missing `text`/`prompt` field; skipping");
                return (HookOutcome::Ok, None);
            };
            (HookOutcome::Block(text.to_string()), None)
        }
        HookKind::Http => {
            let Some(url) = hook.url.as_deref().filter(|s| !s.is_empty()) else {
                warn!("http hook missing `url` field; skipping");
                return (HookOutcome::Ok, None);
            };
            (run_http_hook(http, url, input_json, &hook.headers).await, None)
        }
        HookKind::Agent => {
            let agent_name = hook.agent.as_deref().unwrap_or("unknown");
            debug!("agent hook delegation requested: {agent_name} (no-op)");
            (HookOutcome::Ok, None)
        }
    }
}

async fn run_command_hook(
    command: &str,
    input_json: &str,
    shell: &str,
    env_file_path: Option<&Path>,
    session_id: &str,
    cwd: &str,
) -> (HookOutcome, Option<String>) {
    let mut cmd = Command::new(shell);
    cmd.arg("-c")
        .arg(command)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // §5.3: inject hook environment variables (TS: hooks.ts:815-926)
        .env("CLAUDE_SESSION_ID", session_id)
        .env("CLAUDE_CWD", cwd);

    if let Some(path) = env_file_path {
        cmd.env("CLAUDE_ENV_FILE", path);
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return (HookOutcome::Failed(format!("failed to spawn hook: {e}")), None),
    };

    if let Some(mut stdin) = child.stdin.take() {
        let data = format!("{input_json}\n");
        let _ = stdin.write_all(data.as_bytes()).await;
    }

    let output = match child.wait_with_output().await {
        Ok(o) => o,
        Err(e) => return (HookOutcome::Failed(format!("failed to wait for hook: {e}")), None),
    };

    let exit_code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();

    // Try to parse structured JSON from stdout
    if let Ok(resp) = serde_json::from_str::<HookJsonResponse>(&stdout) {
        let extra = resp
            .hook_specific_output
            .as_ref()
            .and_then(|o| o.additional_context.clone());
        if resp.decision.as_deref() == Some("block") {
            let msg = resp.reason.or(resp.stop_reason).unwrap_or_default();
            return (HookOutcome::Block(msg), extra);
        }
        // Structured response but not blocking — treat as Ok
        return (HookOutcome::Ok, extra);
    }

    if exit_code == 2 {
        (HookOutcome::Block(stdout.trim().to_string()), None)
    } else if exit_code != 0 {
        debug!("hook exited {exit_code}: {command}");
        (HookOutcome::Failed(format!("exit {exit_code}")), None)
    } else {
        (HookOutcome::Ok, None)
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
        if resp.decision.as_deref() == Some("block") {
            let msg = resp.reason.or(resp.stop_reason).unwrap_or_default();
            return HookOutcome::Block(msg);
        }
        return HookOutcome::Ok;
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

/// Build a deduplication key for a hook config.
fn hook_key(h: &HookConfig) -> HookKey {
    HookKey {
        command_or_url: h
            .command
            .clone()
            .or_else(|| h.url.clone())
            .unwrap_or_default(),
        if_condition: h.if_condition.clone(),
        namespace: h
            .extra
            .get("namespace")
            .and_then(|v| v.as_str())
            .map(String::from),
    }
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
    use cc_core::hook::{HookConfig, HookInput, HookKind, HookMatcherGroup, HooksSettings};

    fn test_input(event: &str) -> HookInput {
        HookInput {
            session_id: "test".into(),
            transcript_path: None,
            cwd: "/tmp".into(),
            permission_mode: None,
            hook_event_name: event.into(),
            tool_name: None,
            tool_input: None,
            tool_use_id: None,
            tool_response: None,
            source: None,
            model: None,
            message: None,
            agent_id: None,
            stop_hook_active: None,
            last_assistant_message: None,
        }
    }

    #[test]
    fn hook_config_defaults() {
        let cfg: HookConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(cfg.kind, HookKind::Command);
        assert_eq!(cfg.timeout, 600);
        assert!(!cfg.once);
        assert!(!cfg.is_async);
    }

    #[test]
    fn parses_prompt_hook() {
        // cc-core HookConfig uses `text` field with `prompt` as alias
        let json = r#"{"type":"prompt","text":"You are concise."}"#;
        let cfg: HookConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.kind, HookKind::Prompt);
        assert_eq!(cfg.text.as_deref(), Some("You are concise."));
    }

    #[test]
    fn parses_prompt_hook_alias() {
        // The "prompt" alias also works
        let json = r#"{"type":"prompt","prompt":"Be careful."}"#;
        let cfg: HookConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.kind, HookKind::Prompt);
        assert_eq!(cfg.text.as_deref(), Some("Be careful."));
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
        let json =
            r#"{"matcher":"Bash(git *)","hooks":[{"type":"command","command":"echo hi"}]}"#;
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
        let settings: HooksSettings = serde_json::from_str(
            r#"{
            "PreToolUse": [{"hooks": [{"type": "prompt", "text": "be careful"}]}]
        }"#,
        )
        .unwrap();
        let runner = HookRunner::new(&settings, reqwest::Client::new());
        let input = test_input("PreToolUse");
        let cancel = CancellationToken::new();
        let result = runner.run("PreToolUse", &input, &cancel).await;
        assert!(result.blocked);
        assert_eq!(result.block_message.as_deref(), Some("be careful"));
    }

    #[tokio::test]
    async fn command_hook_exit_2_blocks() {
        let settings: HooksSettings = serde_json::from_str(
            r#"{
            "PreToolUse": [{"hooks": [{"type": "command", "command": "printf 'no go' && exit 2"}]}]
        }"#,
        )
        .unwrap();
        let runner = HookRunner::new(&settings, reqwest::Client::new());
        let input = test_input("PreToolUse");
        let cancel = CancellationToken::new();
        let result = runner.run("PreToolUse", &input, &cancel).await;
        assert!(result.blocked);
        assert_eq!(result.block_message.as_deref(), Some("no go"));
    }

    #[tokio::test]
    async fn once_hook_fires_only_once() {
        let settings: HooksSettings = serde_json::from_str(
            r#"{
            "PreToolUse": [{"hooks": [{"type": "command", "command": "printf 'blocked' && exit 2", "once": true}]}]
        }"#,
        )
        .unwrap();
        let runner = HookRunner::new(&settings, reqwest::Client::new());
        let input = test_input("PreToolUse");
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
        let input = test_input("PreToolUse");
        let cancel = CancellationToken::new();
        let result = runner.run("PreToolUse", &input, &cancel).await;
        assert!(!result.blocked);
    }

    #[tokio::test]
    async fn session_hooks_are_merged() {
        let runner = HookRunner::new(&HooksSettings::new(), reqwest::Client::new());

        runner.add_session_hook(
            "PreToolUse",
            HookMatcherGroup {
                matcher: None,
                hooks: vec![HookConfig {
                    kind: HookKind::Prompt,
                    text: Some("session block".into()),
                    ..Default::default()
                }],
            },
        );

        let input = test_input("PreToolUse");
        let cancel = CancellationToken::new();
        let result = runner.run("PreToolUse", &input, &cancel).await;
        assert!(result.blocked);
        assert_eq!(result.block_message.as_deref(), Some("session block"));
    }

    #[tokio::test]
    async fn command_hook_injects_session_env_vars() {
        // Hook prints CLAUDE_SESSION_ID and CLAUDE_CWD — we check exit code 0
        // to confirm the variables are set (a missing var causes `echo $VAR` to print
        // an empty line, not fail, so we use `test -n "$VAR"` instead).
        let settings: HooksSettings = serde_json::from_str(
            r#"{
            "PreToolUse": [{"hooks": [{"type": "command", "command": "test -n \"$CLAUDE_SESSION_ID\" && test -n \"$CLAUDE_CWD\""}]}]
        }"#,
        )
        .unwrap();
        let runner = HookRunner::new(&settings, reqwest::Client::new());
        let mut input = test_input("PreToolUse");
        input.session_id = "test-session-123".into();
        input.cwd = "/tmp/test".into();
        let cancel = CancellationToken::new();
        let result = runner.run("PreToolUse", &input, &cancel).await;
        // Command exits 0 if both vars are set → not blocked, no failures
        assert!(!result.blocked);
        assert!(result.failures.is_empty(), "failures: {:?}", result.failures);
    }

    #[tokio::test]
    async fn stop_hook_input_has_correct_fields() {
        // Verify HookInput serializes stop_hook_active and last_assistant_message
        let input = HookInput {
            session_id: "sess".into(),
            transcript_path: None,
            cwd: "/".into(),
            permission_mode: None,
            hook_event_name: "Stop".into(),
            tool_name: None,
            tool_input: None,
            tool_use_id: None,
            tool_response: None,
            source: None,
            model: None,
            message: None,
            agent_id: None,
            stop_hook_active: Some(true),
            last_assistant_message: Some("hello".into()),
        };
        let json = serde_json::to_value(&input).unwrap();
        assert_eq!(json["stop_hook_active"], serde_json::json!(true));
        assert_eq!(json["last_assistant_message"], serde_json::json!("hello"));
        assert_eq!(json["hook_event_name"], serde_json::json!("Stop"));
    }

    #[tokio::test]
    async fn command_hook_collects_additional_context() {
        // Hook emits a structured JSON response with hook_specific_output.additional_context.
        // SubagentStart / SessionStart / UserPromptSubmit hooks use this to inject context.
        let settings: HooksSettings = serde_json::from_str(
            r#"{
            "SubagentStart": [{"hooks": [{"type": "command", "command": "printf '{\"hook_specific_output\":{\"additional_context\":\"context from hook\"}}'"}]}]
        }"#,
        )
        .unwrap();
        let runner = HookRunner::new(&settings, reqwest::Client::new());
        let input = test_input("SubagentStart");
        let cancel = CancellationToken::new();
        let result = runner.run("SubagentStart", &input, &cancel).await;
        assert!(!result.blocked);
        assert_eq!(
            result.additional_contexts,
            vec!["context from hook".to_string()]
        );
    }

    #[tokio::test]
    async fn session_end_respects_tight_timeout() {
        // Hook sleeps 10s — session_end enforces 1.5s (or env override).
        // We use a 100ms override via env var to keep the test fast.
        std::env::set_var("CLAUDE_CODE_SESSIONEND_HOOKS_TIMEOUT_MS", "100");
        let settings: HooksSettings = serde_json::from_str(
            r#"{"SessionEnd": [{"hooks": [{"type": "command", "command": "sleep 10", "timeout": 30}]}]}"#,
        )
        .unwrap();
        let runner = HookRunner::new(&settings, reqwest::Client::new());
        let input = test_input("SessionEnd");
        let start = std::time::Instant::now();
        runner.run_session_end(&input).await;
        let elapsed = start.elapsed();
        std::env::remove_var("CLAUDE_CODE_SESSIONEND_HOOKS_TIMEOUT_MS");
        // Should return in ~100ms, well under 1 second
        assert!(elapsed < std::time::Duration::from_secs(1), "took {:?}", elapsed);
    }
}
