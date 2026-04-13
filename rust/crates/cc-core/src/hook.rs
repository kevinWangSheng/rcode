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
    Prompt,  // text injected into context
    Http,    // POST JSON to URL
    Agent,   // delegate to subagent
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

/// A single hook configuration entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookConfig {
    #[serde(rename = "type", default = "default_hook_kind")]
    pub kind: HookKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Prompt text for kind=prompt (also accepts "prompt" key from settings).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "prompt"
    )]
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
    #[serde(default)]
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
}
