use crate::error::CcResult;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// The outcome of a permission check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionBehavior {
    Allow,
    Deny,
    Ask,
}

/// Source of a permission decision (for audit trail).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionSource {
    SettingsAllow,  // matched an allow rule in settings
    SettingsDeny,   // matched a deny rule in settings
    SessionAllow,   // user chose "always allow" earlier this session
    ModeDefault,    // default behavior for current permission mode
    BypassFlag,     // --bypass-permissions CLI flag
    UserPrompt,     // interactive user decision
    Hook,           // hook blocked the action
}

/// Result of evaluating permission rules for a tool invocation.
#[derive(Debug, Clone)]
pub struct PermissionResult {
    pub behavior: PermissionBehavior,
    pub source: PermissionSource,
    pub reason: Option<String>,
}

impl PermissionResult {
    pub fn allow(source: PermissionSource) -> Self {
        Self {
            behavior: PermissionBehavior::Allow,
            source,
            reason: None,
        }
    }

    pub fn deny(source: PermissionSource, reason: impl Into<String>) -> Self {
        Self {
            behavior: PermissionBehavior::Deny,
            source,
            reason: Some(reason.into()),
        }
    }

    pub fn ask() -> Self {
        Self {
            behavior: PermissionBehavior::Ask,
            source: PermissionSource::ModeDefault,
            reason: None,
        }
    }
}

/// A parsed permission rule (from settings.json allow/deny lists).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PermissionRule {
    /// Simple string pattern: "Bash", "Bash(*)", "Write(**)", "mcp__*"
    Simple(String),
    /// Structured rule: { tool: "Bash", input: { command: "git *" } }
    Structured {
        tool: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<Value>,
    },
}

/// User's decision when prompted for permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptDecision {
    Allow,       // allow this one invocation
    AllowAlways, // allow this tool for the rest of the session
    Deny,        // deny this invocation (is_error: true)
}

/// The permission prompter interface — implemented by TUI and headless modes.
#[async_trait::async_trait]
pub trait PermissionPrompter: Send + Sync {
    async fn prompt(
        &self,
        tool_name: &str,
        tool_input: &Value,
        cancel: &CancellationToken,
    ) -> CcResult<PromptDecision>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_rule_simple_deserialization() {
        let rule: PermissionRule = serde_json::from_str("\"Bash(*)\"").unwrap();
        assert!(matches!(rule, PermissionRule::Simple(s) if s == "Bash(*)"));
    }

    #[test]
    fn permission_rule_structured_deserialization() {
        let json = r#"{"tool": "Bash", "input": {"command": "git *"}}"#;
        let rule: PermissionRule = serde_json::from_str(json).unwrap();
        match rule {
            PermissionRule::Structured { tool, input } => {
                assert_eq!(tool, "Bash");
                assert!(input.is_some());
            }
            _ => panic!("expected structured rule"),
        }
    }

    #[test]
    fn permission_result_constructors() {
        let allow = PermissionResult::allow(PermissionSource::BypassFlag);
        assert_eq!(allow.behavior, PermissionBehavior::Allow);
        assert_eq!(allow.source, PermissionSource::BypassFlag);

        let deny = PermissionResult::deny(PermissionSource::SettingsDeny, "blocked");
        assert_eq!(deny.behavior, PermissionBehavior::Deny);
        assert_eq!(deny.reason, Some("blocked".to_string()));
    }
}
