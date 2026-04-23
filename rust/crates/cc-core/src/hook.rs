use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// All hook event names (from TS source).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum HookEvent {
    // Tool lifecycle
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
    // Permission decision points
    PermissionRequest,
    PermissionDenied,
    // Notification
    Notification,
    // Session lifecycle
    SessionStart,
    SessionStop,
    // Query lifecycle
    PreApiCall,
    PostApiCall,
    // Model output
    ModelResponse,
    // Subagent
    SubagentStart,
    SubagentStop,
}

/// Hook execution type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum HookKind {
    #[default]
    Command, // shell command, JSON on stdin
    Prompt, // text injected into context
    Http,   // POST JSON to URL
    Agent,  // delegate to subagent
}

/// Matcher for filtering which tool invocations trigger a hook.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookMatcher {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>, // glob pattern
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_contains: Option<String>, // substring match on input JSON
}

fn default_hook_kind() -> HookKind {
    HookKind::Command
}
fn default_hook_timeout() -> u64 {
    600
}

/// Command-kind hook invocation form — either an argv vector (no shell) or
/// a raw string intended for a shell-of-your-choice `-c` invocation.
///
/// The array form is strongly preferred: it skips the shell entirely, so
/// `$(…)` / backticks / `>` / `;` embedded in the command don't
/// metacharacter their way into arbitrary code execution. The string form
/// is kept for backwards-compatibility only — it requires an explicit
/// `unsafe_shell: true` sibling field (see [`HookConfig::unsafe_shell`]) to
/// run. See `openspec/changes/fix-hook-command-injection/proposal.md` for
/// the full rationale.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum HookCommand {
    /// Preferred. Spawned as `Command::new(argv[0]).args(&argv[1..])` —
    /// the OS-level exec, not a shell.
    Argv(Vec<String>),
    /// Legacy. Must be paired with `unsafe_shell: true` or hook loading
    /// refuses to run it. Piped through `<shell> -c <string>`.
    Shell(String),
}

impl HookCommand {
    /// `true` if this form requires `unsafe_shell: true` to be allowed.
    pub fn requires_unsafe_shell(&self) -> bool {
        matches!(self, HookCommand::Shell(_))
    }

    /// Best-effort preview for diagnostic messages — never meant to be
    /// parsed or round-tripped, just something a user can eyeball.
    pub fn preview(&self) -> String {
        match self {
            HookCommand::Argv(argv) => argv.join(" "),
            HookCommand::Shell(s) => s.clone(),
        }
    }
}

/// A single hook configuration entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookConfig {
    #[serde(rename = "type", default = "default_hook_kind")]
    pub kind: HookKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<HookCommand>,
    /// Opt-in gate for the string-form [`HookCommand::Shell`]. If
    /// `command` is a string and this is `false`, hook loading refuses
    /// to run it — the user must either migrate to the array form or
    /// explicitly acknowledge the shell-injection surface by flipping
    /// this to `true`. Defaults to `false` so the safe path is
    /// default-on.
    #[serde(default)]
    pub unsafe_shell: bool,
    /// Prompt text for kind=prompt (also accepts "prompt" key from settings).
    #[serde(default, skip_serializing_if = "Option::is_none", alias = "prompt")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default = "default_hook_timeout")]
    pub timeout: u64, // seconds
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matcher: Option<HookMatcher>,
    #[serde(rename = "if", default, skip_serializing_if = "Option::is_none")]
    pub if_condition: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_message: Option<String>,
    #[serde(default)]
    pub once: bool,
    #[serde(rename = "async", default)]
    pub is_async: bool,
    #[serde(default, alias = "asyncRewake")]
    pub async_rewake: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<HashMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_env_vars: Option<Vec<String>>,
    /// Forward-compat: preserve unknown fields.
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

impl Default for HookConfig {
    fn default() -> Self {
        Self {
            kind: HookKind::Command,
            command: None,
            unsafe_shell: false,
            text: None,
            url: None,
            agent: None,
            timeout: 600,
            matcher: None,
            if_condition: None,
            shell: None,
            status_message: None,
            once: false,
            is_async: false,
            async_rewake: false,
            headers: None,
            allowed_env_vars: None,
            extra: HashMap::new(),
        }
    }
}

/// A matcher group: event + matcher pattern + list of hooks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookMatcherGroup {
    pub matcher: Option<String>,
    pub hooks: Vec<HookConfig>,
}

/// All hooks for all events, as stored in settings.json.
pub type HooksSettings = HashMap<String, Vec<HookMatcherGroup>>;

/// Result of running a single hook.
#[derive(Debug, Clone)]
pub enum HookOutcome {
    Ok,
    Block(String),  // exit 2 or {"block": true}
    Failed(String), // non-blocking error
    /// Hook opted into `async_rewake: true` and exited 2 — engine should
    /// treat the message as a rewake signal rather than a plain block.
    /// Currently aggregated alongside `Block`; a task-notification queue
    /// in cc-query will later promote this into a re-entry.
    AsyncRewake(String),
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

/// Data sent to hooks on stdin as JSON.
#[derive(Debug, Clone, Serialize)]
pub struct HookInput {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
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
    /// Stop hook: whether a stop hook is already active (prevents recursion).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_hook_active: Option<bool>,
    /// Stop hook: last assistant message text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_assistant_message: Option<String>,
}

impl HookInput {
    /// Construct a HookInput with `cwd` populated from the current directory
    /// and every optional field set to `None`. Callers chain the `with_*`
    /// setters to fill in event-specific fields.
    ///
    /// This exists because HookInput has 15 fields and the 9+ construction
    /// sites otherwise repeat ~12 `None`s each, obscuring which fields the
    /// event actually uses.
    pub fn base(session_id: impl Into<String>, event: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            transcript_path: None,
            cwd: std::env::current_dir()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
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

    /// Populate `tool_name`, `tool_input`, and `tool_use_id` from a
    /// `ToolUseBlock` — the pattern every tool-lifecycle hook uses.
    pub fn with_tool(mut self, tu: &crate::ToolUseBlock) -> Self {
        self.tool_name = Some(tu.name.clone());
        self.tool_input = Some(tu.input.clone());
        self.tool_use_id = Some(tu.id.clone());
        self
    }

    pub fn with_transcript_path(mut self, path: impl Into<String>) -> Self {
        self.transcript_path = Some(path.into());
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }

    pub fn with_agent_id(mut self, agent_id: impl Into<String>) -> Self {
        self.agent_id = Some(agent_id.into());
        self
    }

    pub fn with_stop_hook_active(mut self, active: bool) -> Self {
        self.stop_hook_active = Some(active);
        self
    }

    pub fn with_last_assistant_message(mut self, last: Option<impl Into<String>>) -> Self {
        self.last_assistant_message = last.map(Into::into);
        self
    }

    pub fn with_tool_response(mut self, response: Option<Value>) -> Self {
        self.tool_response = response;
        self
    }

    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }

    /// Override cwd (defaults to `current_dir`). Needed by sub-agent runners
    /// that capture the cwd before spawning.
    pub fn with_cwd(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = cwd.into();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_config_defaults() {
        let json = r#"{"command": "echo hello"}"#;
        let config: HookConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.kind, HookKind::Command);
        assert_eq!(config.timeout, 600);
        assert!(!config.once);
        assert!(!config.is_async);
    }

    #[test]
    fn hook_event_pascal_case_serialization() {
        assert_eq!(
            serde_json::to_string(&HookEvent::PreToolUse).unwrap(),
            "\"PreToolUse\""
        );
        assert_eq!(
            serde_json::to_string(&HookEvent::SessionStart).unwrap(),
            "\"SessionStart\""
        );
        assert_eq!(
            serde_json::to_string(&HookEvent::PostApiCall).unwrap(),
            "\"PostApiCall\""
        );
        let event: HookEvent = serde_json::from_str("\"SubagentStop\"").unwrap();
        assert_eq!(event, HookEvent::SubagentStop);
    }

    #[test]
    fn hook_config_with_type() {
        let json = r#"{"type": "http", "url": "https://example.com/hook", "timeout": 30}"#;
        let config: HookConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.kind, HookKind::Http);
        assert_eq!(config.timeout, 30);
        assert_eq!(config.url.as_deref(), Some("https://example.com/hook"));
    }

    #[test]
    fn async_rewake_parses_both_snake_and_camel() {
        let snake = r#"{"command": "echo", "async_rewake": true}"#;
        let camel = r#"{"command": "echo", "asyncRewake": true}"#;
        let from_snake: HookConfig = serde_json::from_str(snake).unwrap();
        let from_camel: HookConfig = serde_json::from_str(camel).unwrap();
        assert!(from_snake.async_rewake);
        assert!(from_camel.async_rewake);
    }
}
