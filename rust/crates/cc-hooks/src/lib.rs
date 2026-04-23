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
    HookConfig, HookInput, HookJsonResponse, HookKind, HookMatcherGroup, HookOutcome, HooksSettings,
};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

pub mod http;

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
    /// Set when a hook with `async_rewake: true` exits 2. Distinct from
    /// `block_message` so a future task-notification queue can promote this
    /// into a re-entry rather than a tool-result error. For now cc-query
    /// treats it like `blocked` (tool-result error).
    pub async_rewake: Option<String>,
}

/// Extra environment the runner exports to every hook child process.
///
/// Mirrors TS `src/utils/hooks.ts:881-926`:
/// * `CLAUDE_PROJECT_DIR` — workspace root.
/// * `CLAUDE_PLUGIN_ROOT` — source dir of the plugin that registered the hook.
/// * `CLAUDE_PLUGIN_DATA` — writable per-plugin data dir.
/// * `CLAUDE_PLUGIN_OPTION_*` — each option is uppercased + non-ident chars
///   replaced with `_`, then stringified. Matches TS `hooks.ts:898-906`.
///
/// All fields are optional so the most common case (plain project hook, no
/// plugin) needs no boilerplate.
#[derive(Debug, Clone, Default)]
pub struct HookContext {
    pub project_dir: Option<PathBuf>,
    pub plugin_root: Option<PathBuf>,
    pub plugin_data: Option<PathBuf>,
    pub plugin_options: HashMap<String, String>,
}

impl HookContext {
    /// Construct the env-var pairs this context contributes to a hook child
    /// process. Key order: `CLAUDE_PROJECT_DIR`, `CLAUDE_PLUGIN_ROOT`,
    /// `CLAUDE_PLUGIN_DATA`, then `CLAUDE_PLUGIN_OPTION_*` sorted by key.
    pub fn env_pairs(&self) -> Vec<(String, String)> {
        let mut pairs = Vec::new();
        if let Some(dir) = &self.project_dir {
            pairs.push((
                "CLAUDE_PROJECT_DIR".to_string(),
                dir.to_string_lossy().into_owned(),
            ));
        }
        if let Some(root) = &self.plugin_root {
            pairs.push((
                "CLAUDE_PLUGIN_ROOT".to_string(),
                root.to_string_lossy().into_owned(),
            ));
        }
        if let Some(data) = &self.plugin_data {
            pairs.push((
                "CLAUDE_PLUGIN_DATA".to_string(),
                data.to_string_lossy().into_owned(),
            ));
        }
        let mut opts: Vec<(&String, &String)> = self.plugin_options.iter().collect();
        opts.sort_by(|a, b| a.0.cmp(b.0));
        for (k, v) in opts {
            pairs.push((plugin_option_env_key(k), v.clone()));
        }
        pairs
    }
}

/// Build the `CLAUDE_PLUGIN_OPTION_*` name for a single option key.
///
/// Matches TS `src/utils/hooks.ts:903-904`:
/// ```ts
/// const envKey = key.replace(/[^A-Za-z0-9_]/g, '_').toUpperCase()
/// ```
pub fn plugin_option_env_key(key: &str) -> String {
    let mut out = String::with_capacity("CLAUDE_PLUGIN_OPTION_".len() + key.len());
    out.push_str("CLAUDE_PLUGIN_OPTION_");
    for ch in key.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch.to_ascii_uppercase());
        } else {
            out.push('_');
        }
    }
    out
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
    /// Extra env vars to export into every hook child process (project dir,
    /// plugin root/data/options).
    context: HookContext,
}

impl HookRunner {
    /// Snapshot the config at session start (prevents race with settings changes).
    pub fn new(settings: &HooksSettings, http: reqwest::Client) -> Self {
        // §3.1 fix-hook-command-injection: emit a single startup summary of
        // every `unsafe_shell: true` hook currently loaded. Per-hook failures
        // already fire at invocation time; this summary gives an operator one
        // consolidated place to audit the opt-in shell surface at session start.
        log_unsafe_shell_summary(settings);
        Self {
            config_snapshot: settings.clone(),
            session_hooks: RwLock::new(HooksSettings::new()),
            fired_once: Mutex::new(HashSet::new()),
            http,
            managed_only: false,
            disabled: false,
            context: HookContext::default(),
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
            context: HookContext::default(),
        }
    }

    /// Attach a `HookContext` that the runner exports into each hook's
    /// child process. Replaces any previously-set context.
    pub fn with_context(mut self, context: HookContext) -> Self {
        self.context = context;
        self
    }

    /// Read-only access to the current context.
    pub fn context(&self) -> &HookContext {
        &self.context
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
        let plugin_env: Arc<Vec<(String, String)>> = Arc::new(self.context.env_pairs());

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
                let plugin_env = plugin_env.clone();
                async move {
                    let timeout = Duration::from_secs(h.timeout);
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => (HookOutcome::Failed("cancelled".into()), None),
                        result = tokio::time::timeout(
                            timeout,
                            execute_one_hook(h, &input_json, &http, env_path.as_deref(), &sid, &cwd, &plugin_env),
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
                HookOutcome::AsyncRewake(msg) => {
                    // TS parity deferred: until cc-query grows a task-notification
                    // queue we treat rewake the same as Block at the consumer —
                    // but surface the distinct field so the queue can be wired
                    // later without another type change.
                    result.blocked = true;
                    result.block_message = Some(msg.clone());
                    result.async_rewake = Some(msg);
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
    plugin_env: &[(String, String)],
) -> (HookOutcome, Option<String>) {
    match &hook.kind {
        HookKind::Command => {
            let Some(command) = hook.command.as_ref() else {
                return (HookOutcome::Ok, None); // silently skip
            };
            match command {
                cc_core::hook::HookCommand::Argv(argv) if !argv.is_empty() => {
                    run_argv_hook(
                        argv,
                        input_json,
                        env_file_path,
                        session_id,
                        cwd,
                        plugin_env,
                        hook.async_rewake,
                    )
                    .await
                }
                cc_core::hook::HookCommand::Argv(_) => {
                    // Empty argv — treat as unconfigured.
                    (HookOutcome::Ok, None)
                }
                cc_core::hook::HookCommand::Shell(s) if s.is_empty() => (HookOutcome::Ok, None),
                cc_core::hook::HookCommand::Shell(s) => {
                    if !hook.unsafe_shell {
                        // Gate the shell-injection surface. The user can opt
                        // in by adding `"unsafe_shell": true` to the hook
                        // entry — but most hooks should just migrate to the
                        // array form. See fix-hook-command-injection.
                        return (
                            HookOutcome::Failed(
                                "hook: string-form `command` requires `unsafe_shell: true` \
                                 in the same entry. Prefer migrating to `command: [\"argv[0]\", \
                                 \"argv[1]\", ...]` which skips the shell entirely."
                                    .into(),
                            ),
                            None,
                        );
                    }
                    let shell = hook.shell.as_deref().unwrap_or("bash");
                    run_command_hook(
                        s,
                        input_json,
                        shell,
                        env_file_path,
                        session_id,
                        cwd,
                        plugin_env,
                        hook.async_rewake,
                    )
                    .await
                }
            }
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
            (
                run_http_hook(http, url, input_json, &hook.headers).await,
                None,
            )
        }
        HookKind::Agent => {
            let agent_name = hook.agent.as_deref().unwrap_or("unknown");
            debug!("agent hook delegation requested: {agent_name} (no-op)");
            (HookOutcome::Ok, None)
        }
    }
}

/// Run a hook in argv form — no intervening shell. `argv[0]` is the
/// executable, `argv[1..]` are literal args. Nothing is interpreted.
#[allow(clippy::too_many_arguments)]
async fn run_argv_hook(
    argv: &[String],
    input_json: &str,
    env_file_path: Option<&Path>,
    session_id: &str,
    cwd: &str,
    plugin_env: &[(String, String)],
    async_rewake: bool,
) -> (HookOutcome, Option<String>) {
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    run_prepared_hook(
        cmd,
        input_json,
        env_file_path,
        session_id,
        cwd,
        plugin_env,
        async_rewake,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_command_hook(
    command: &str,
    input_json: &str,
    shell: &str,
    env_file_path: Option<&Path>,
    session_id: &str,
    cwd: &str,
    plugin_env: &[(String, String)],
    async_rewake: bool,
) -> (HookOutcome, Option<String>) {
    let mut cmd = Command::new(shell);
    cmd.arg("-c").arg(command);
    run_prepared_hook(
        cmd,
        input_json,
        env_file_path,
        session_id,
        cwd,
        plugin_env,
        async_rewake,
    )
    .await
}

/// Inner runner that takes a pre-configured `Command` (argv or `sh -c`)
/// and runs the common stdin-feed / wait / parse-output pipeline.
#[allow(clippy::too_many_arguments)]
async fn run_prepared_hook(
    mut cmd: Command,
    input_json: &str,
    env_file_path: Option<&Path>,
    session_id: &str,
    cwd: &str,
    plugin_env: &[(String, String)],
    async_rewake: bool,
) -> (HookOutcome, Option<String>) {
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // §5.3: inject hook environment variables (TS: hooks.ts:815-926)
        .env("CLAUDE_SESSION_ID", session_id)
        .env("CLAUDE_CWD", cwd);

    for (k, v) in plugin_env {
        cmd.env(k, v);
    }

    if let Some(path) = env_file_path {
        cmd.env("CLAUDE_ENV_FILE", path);
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return (
                HookOutcome::Failed(format!("failed to spawn hook: {e}")),
                None,
            )
        }
    };

    // Feed the hook its JSON input via stdin. A failed write here is the
    // classic "hook silently got half a JSON" bug: the child's own parser
    // then fails and the user sees "unexpected EOF" in the hook's stderr
    // with no breadcrumb pointing at our write. Capture the error and,
    // after reaping the child, surface it as a structured failure instead
    // of letting a misleading exit code through.
    let mut stdin_err: Option<String> = None;
    if let Some(mut stdin) = child.stdin.take() {
        let data = format!("{input_json}\n");
        if let Err(e) = stdin.write_all(data.as_bytes()).await {
            stdin_err = Some(e.to_string());
        }
    }

    let output = match child.wait_with_output().await {
        Ok(o) => o,
        Err(e) => {
            return (
                HookOutcome::Failed(format!("failed to wait for hook: {e}")),
                None,
            )
        }
    };

    // If the stdin write failed, the hook can't have run meaningfully —
    // its input was truncated. Surface the structured cause regardless of
    // what exit code the child chose.
    if let Some(err) = stdin_err {
        return (HookOutcome::Failed(format!("stdin_write: {err}")), None);
    }

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
            if async_rewake {
                return (HookOutcome::AsyncRewake(msg), extra);
            }
            return (HookOutcome::Block(msg), extra);
        }
        // Structured response but not blocking — treat as Ok
        return (HookOutcome::Ok, extra);
    }

    if exit_code == 2 {
        let msg = stdout.trim().to_string();
        if async_rewake {
            (HookOutcome::AsyncRewake(msg), None)
        } else {
            (HookOutcome::Block(msg), None)
        }
    } else if exit_code != 0 {
        // The refactor to shared argv/shell runner means we no longer have
        // a `command` identifier here — the caller knows what it ran.
        debug!("hook exited {exit_code}");
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
        // Run every header through env-var interpolation + CR/LF/NUL
        // rejection before attaching. Defends against CRLF-injection
        // via crafted `${SECRET}` values and matches TS parity
        // (execHttpHook.ts:76-108 with a fail-loud twist — see http.rs).
        let env: HashMap<String, String> = std::env::vars().collect();
        let prepared = match http::prepare_headers(headers, &env, None) {
            Ok(p) => p,
            Err(e) => {
                return HookOutcome::Failed(format!("http hook header prep: {e}"));
            }
        };
        for (k, v) in prepared {
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

/// Emit one consolidated WARN at session start summarising every hook
/// configured with a string-form `command` + `unsafe_shell: true`. This is a
/// follow-up to fix-hook-command-injection §3.1: per-invocation failures are
/// loud but per-startup visibility makes the shell-injection surface easy to
/// audit without digging through invocation logs. Fires exactly once per
/// `HookRunner::new` call; truncates to the first 10 entries to keep logs
/// readable if someone has dozens of legacy hooks.
fn log_unsafe_shell_summary(settings: &HooksSettings) {
    const MAX_ENTRIES: usize = 10;
    let mut entries: Vec<String> = Vec::new();
    let mut total: usize = 0;
    for (event, groups) in settings.iter() {
        for group in groups {
            for hook in &group.hooks {
                let is_shell_unsafe = matches!(
                    hook.command.as_ref(),
                    Some(cc_core::hook::HookCommand::Shell(_))
                ) && hook.unsafe_shell;
                if !is_shell_unsafe {
                    continue;
                }
                total += 1;
                if entries.len() < MAX_ENTRIES {
                    let preview = hook
                        .command
                        .as_ref()
                        .map(|c| c.preview())
                        .unwrap_or_default();
                    // Trim overly long commands so a single huge one-liner
                    // doesn't blow up the log line.
                    let preview = if preview.len() > 120 {
                        format!("{}…", &preview[..120])
                    } else {
                        preview
                    };
                    entries.push(format!("{event}:{preview}"));
                }
            }
        }
    }
    if total == 0 {
        return;
    }
    let listed = entries.join(", ");
    let overflow = total.saturating_sub(entries.len());
    if overflow > 0 {
        warn!(
            "[hooks] {total} string-form command(s) using unsafe_shell=true: [{listed}, and {overflow} more]"
        );
    } else {
        warn!("[hooks] {total} string-form command(s) using unsafe_shell=true: [{listed}]");
    }
}

/// Build a deduplication key for a hook config.
fn hook_key(h: &HookConfig) -> HookKey {
    // For dedup purposes collapse both HookCommand variants into a stable
    // preview string. Two different shapes that happen to preview the same
    // still dedup — fine, because they'd also be identical to a user
    // reading the settings file.
    let command_preview = h.command.as_ref().map(|c| c.preview());
    HookKey {
        command_or_url: command_preview
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
    fn parses_command_as_argv_array() {
        let json = r#"{"type":"command","command":["/bin/echo","hello","world"]}"#;
        let cfg: HookConfig = serde_json::from_str(json).unwrap();
        match cfg.command.as_ref().expect("command set") {
            cc_core::hook::HookCommand::Argv(argv) => {
                assert_eq!(argv, &["/bin/echo", "hello", "world"]);
            }
            other => panic!("expected Argv, got {other:?}"),
        }
        assert!(!cfg.unsafe_shell, "argv form doesn't need unsafe_shell");
    }

    #[test]
    fn parses_command_as_shell_string() {
        let json = r#"{"type":"command","command":"echo hi","unsafe_shell":true}"#;
        let cfg: HookConfig = serde_json::from_str(json).unwrap();
        match cfg.command.as_ref().expect("command set") {
            cc_core::hook::HookCommand::Shell(s) => {
                assert_eq!(s, "echo hi");
            }
            other => panic!("expected Shell, got {other:?}"),
        }
        assert!(cfg.unsafe_shell);
    }

    #[tokio::test]
    async fn argv_hook_runs_without_shell() {
        let settings: HooksSettings = serde_json::from_str(
            r#"{
            "PreToolUse": [{"hooks": [{"type": "command", "command": ["/bin/sh", "-c", "exit 2"]}]}]
        }"#,
        )
        .unwrap();
        let runner = HookRunner::new(&settings, reqwest::Client::new());
        let input = test_input("PreToolUse");
        let cancel = CancellationToken::new();
        let result = runner.run("PreToolUse", &input, &cancel).await;
        assert!(result.blocked, "exit 2 → block");
    }

    #[tokio::test]
    async fn string_command_without_unsafe_shell_is_rejected() {
        // Default `unsafe_shell: false` — the runner must refuse to exec
        // the string form and record a failure, not silently run it.
        let settings: HooksSettings = serde_json::from_str(
            r#"{
            "PreToolUse": [{"hooks": [{"type": "command", "command": "echo danger; rm -rf /"}]}]
        }"#,
        )
        .unwrap();
        let runner = HookRunner::new(&settings, reqwest::Client::new());
        let input = test_input("PreToolUse");
        let cancel = CancellationToken::new();
        let result = runner.run("PreToolUse", &input, &cancel).await;
        // Not blocked (block is a semantic decision the hook never got to make).
        // Must record as failure so the caller can surface the gate.
        assert!(!result.blocked);
        assert!(
            result.failures.iter().any(|f| f.contains("unsafe_shell")),
            "expected an unsafe_shell failure, got {:?}",
            result.failures
        );
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
            "PreToolUse": [{"hooks": [{"type": "command", "command": "printf 'no go' && exit 2", "unsafe_shell": true}]}]
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
            "PreToolUse": [{"hooks": [{"type": "command", "command": "printf 'blocked' && exit 2", "once": true, "unsafe_shell": true}]}]
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
            "PreToolUse": [{"hooks": [{"type": "command", "command": "test -n \"$CLAUDE_SESSION_ID\" && test -n \"$CLAUDE_CWD\"", "unsafe_shell": true}]}]
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
        assert!(
            result.failures.is_empty(),
            "failures: {:?}",
            result.failures
        );
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
            "SubagentStart": [{"hooks": [{"type": "command", "command": "printf '{\"hook_specific_output\":{\"additional_context\":\"context from hook\"}}'", "unsafe_shell": true}]}]
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

    // §3.1 fix-hook-command-injection: the startup summary fires exactly
    // when at least one unsafe_shell hook is present, and stays silent
    // otherwise. Uses `tracing_test` so the log assertion runs in-process.
    #[test]
    #[tracing_test::traced_test]
    fn startup_warn_fires_for_unsafe_shell_hook() {
        let settings: HooksSettings = serde_json::from_str(
            r#"{
            "PreToolUse": [{"hooks": [{"type": "command", "command": "echo hi", "unsafe_shell": true}]}]
        }"#,
        )
        .unwrap();
        let _runner = HookRunner::new(&settings, reqwest::Client::new());
        assert!(
            logs_contain("string-form command(s) using unsafe_shell=true"),
            "expected consolidated WARN summary when an unsafe_shell hook is loaded"
        );
        assert!(
            logs_contain("PreToolUse:echo hi"),
            "summary must list event:command preview for each unsafe_shell entry"
        );
    }

    #[test]
    #[tracing_test::traced_test]
    fn startup_warn_silent_when_no_unsafe_shell_hooks() {
        // Argv form never needs unsafe_shell, so the summary must not fire.
        let settings: HooksSettings = serde_json::from_str(
            r#"{
            "PreToolUse": [{"hooks": [{"type": "command", "command": ["/bin/echo", "hi"]}]}]
        }"#,
        )
        .unwrap();
        let _runner = HookRunner::new(&settings, reqwest::Client::new());
        assert!(
            !logs_contain("string-form command(s) using unsafe_shell"),
            "summary must not fire when no unsafe_shell hooks exist"
        );
    }

    #[test]
    #[tracing_test::traced_test]
    fn startup_warn_truncates_after_ten_entries() {
        // Build 12 unsafe_shell hooks — summary should list 10 and note 2 more.
        let mut settings = HooksSettings::new();
        let hooks: Vec<HookConfig> = (0..12)
            .map(|i| HookConfig {
                kind: HookKind::Command,
                command: Some(cc_core::hook::HookCommand::Shell(format!("echo {i}"))),
                unsafe_shell: true,
                ..Default::default()
            })
            .collect();
        settings.insert(
            "PreToolUse".to_string(),
            vec![HookMatcherGroup {
                matcher: None,
                hooks,
            }],
        );
        let _runner = HookRunner::new(&settings, reqwest::Client::new());
        assert!(logs_contain("12 string-form command(s)"));
        assert!(
            logs_contain("and 2 more"),
            "overflow tail must be logged when >10 entries"
        );
    }

    // §3.1 fix-hook-stdin-error-propagation: deterministic E2E test that the
    // parent's stdin `write_all` error surfaces as `HookOutcome::Failed` with a
    // `stdin_write:` prefix. Earlier flaky attempts raced a small write against
    // a hook that closed stdin, but on fast hardware the write often slipped
    // into the kernel pipe buffer before the child exited, so no EPIPE ever
    // fired. The deterministic fix: pick a hook command that `exec 0<&-`
    // closes its own stdin *before* doing anything else, and ship a payload
    // far larger than any plausible pipe buffer (macOS default ≈16–64 KiB,
    // Linux ≈64 KiB). ~2 MiB guarantees the write saturates and blocks, at
    // which point the closed reader forces a `BrokenPipe`.
    #[tokio::test]
    async fn stdin_write_failure_surfaces_as_structured_failure() {
        // Hook closes stdin immediately, then exits 0. Because the reader end
        // of the pipe is gone before the parent's multi-MiB write can drain,
        // `write_all` MUST fail with BrokenPipe.
        let settings: HooksSettings = serde_json::from_str(
            r#"{
            "PreToolUse": [{"hooks": [{"type": "command", "command": "exec 0<&-; exit 0", "unsafe_shell": true, "timeout": 10}]}]
        }"#,
        )
        .unwrap();
        let runner = HookRunner::new(&settings, reqwest::Client::new());
        let mut input = test_input("PreToolUse");
        // Stuff the serialized HookInput with a payload large enough to
        // overflow any realistic pipe buffer (typically 16–64 KiB). 2 MiB
        // removes all timing ambiguity on both macOS and Linux.
        input.message = Some("x".repeat(2 * 1024 * 1024));
        let cancel = CancellationToken::new();
        let result = runner.run("PreToolUse", &input, &cancel).await;

        // Not blocked — the child never got to make a semantic decision.
        assert!(
            !result.blocked,
            "stdin_write failure must not be interpreted as block: {:?}",
            result.block_message
        );
        // Exactly one failure, tagged with the structured `stdin_write:` prefix.
        assert_eq!(
            result.failures.len(),
            1,
            "expected one failure, got {:?}",
            result.failures
        );
        assert!(
            result.failures[0].starts_with("stdin_write:"),
            "failure must carry the `stdin_write:` prefix so callers can \
             distinguish this from a generic hook error; got {:?}",
            result.failures[0]
        );
    }

    #[tokio::test]
    async fn session_end_respects_tight_timeout() {
        // Hook sleeps 10s — session_end enforces 1.5s (or env override).
        // We use a 100ms override via env var to keep the test fast.
        std::env::set_var("CLAUDE_CODE_SESSIONEND_HOOKS_TIMEOUT_MS", "100");
        let settings: HooksSettings = serde_json::from_str(
            r#"{"SessionEnd": [{"hooks": [{"type": "command", "command": "sleep 10", "timeout": 30, "unsafe_shell": true}]}]}"#,
        )
        .unwrap();
        let runner = HookRunner::new(&settings, reqwest::Client::new());
        let input = test_input("SessionEnd");
        let start = std::time::Instant::now();
        runner.run_session_end(&input).await;
        let elapsed = start.elapsed();
        std::env::remove_var("CLAUDE_CODE_SESSIONEND_HOOKS_TIMEOUT_MS");
        // Should return in ~100ms, well under 1 second
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "took {:?}",
            elapsed
        );
    }
}
