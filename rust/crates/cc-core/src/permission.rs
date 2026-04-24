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
    SettingsAllow, // matched an allow rule in settings
    SettingsDeny,  // matched a deny rule in settings
    SessionAllow,  // user chose "always allow" earlier this session
    ModeDefault,   // default behavior for current permission mode
    BypassFlag,    // --bypass-permissions CLI flag
    UserPrompt,    // interactive user decision
    Hook,          // hook blocked the action
    ToolCheck,     // tool-specific `Tool::check_permissions` hook (Change B)
    SafetyCheck,   // bypass-immune write to `.git/` / `.claude/` / shell configs
    Classifier,    // auto-mode yoloClassifier decision
}

/// Structured decision-reason for richer audit / UI (2026-04-24 parity-gaps,
/// roadmap P2 #48). Mirrors TS `DecisionReason` union. The existing
/// [`PermissionSource`] is a coarser single-byte bucket; `decision_reason`
/// lets the dialog surface the *specific* rule that matched, the mode that
/// forced the decision, or the tool/hook that intervened.
#[derive(Debug, Clone, PartialEq)]
pub enum PermissionDecisionReason {
    /// A settings-layer allow/deny rule matched.
    Rule(PermissionRule),
    /// The current mode produced the decision (e.g. `Plan` auto-denies
    /// write tools).
    Mode(PermissionMode),
    /// A hook (`PermissionRequest`) vetoed / overrode.
    Hook { name: String },
    /// Bypass-immune safety check (`.git/`, `.claude/`, shell config).
    SafetyCheck { path: String },
    /// Auto-mode classifier verdict.
    Classifier { category: String },
    /// Free-form fallback for callers that don't fit the buckets above.
    Other(String),
}

/// Where a `PermissionUpdate` writes its rules. Mirrors TS
/// `PermissionUpdateDestination`. `Session` is memory-only; the rest
/// correspond to settings.json layers picked up by cc-config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionDestination {
    Session,
    UserSettings,
    ProjectSettings,
    LocalSettings,
    Flag,
    Policy,
    Cli,
}

/// An actionable rule delta that the dialog can offer ("Allow always",
/// "Deny always") and the engine can apply by either updating session
/// state (Session) or persisting to settings (UserSettings / …). Carried
/// on [`PermissionResult::suggestions`]. Matches TS
/// `PermissionUpdate`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionUpdate {
    pub destination: PermissionDestination,
    pub rules: Vec<PermissionRule>,
    pub behavior: PermissionBehavior,
}

/// Result of evaluating permission rules for a tool invocation.
///
/// The minimal fields (`behavior`, `source`, `reason`) carry today's
/// 5-stage decision. The richer fields are used by follow-up changes
/// (tool `check_permissions`, hook-backed decision overrides, "Allow
/// always" persistence) without breaking existing call sites — they
/// default to empty / None / false via
/// [`PermissionResult::allow`] / [`PermissionResult::deny`] /
/// [`PermissionResult::ask`].
#[derive(Debug, Clone)]
pub struct PermissionResult {
    pub behavior: PermissionBehavior,
    pub source: PermissionSource,
    pub reason: Option<String>,
    /// Rule deltas the dialog can offer as one-click follow-ups.
    pub suggestions: Vec<PermissionUpdate>,
    /// Tool-rewritten input (redacted secrets, normalised paths).
    /// `None` means "use the original input unchanged."
    pub updated_input: Option<Value>,
    /// Structured audit-source. `Some` complements `source` with a
    /// more specific description of *why* the decision was made.
    pub decision_reason: Option<PermissionDecisionReason>,
    /// If true, the agent must abort the whole turn — not just the
    /// current tool. Used by hooks that detect prompt-injection /
    /// out-of-band policy violations.
    pub interrupt: bool,
    /// Tool_result text shown to the model. Distinct from `reason`,
    /// which is audit-facing.
    pub message: Option<String>,
}

impl PermissionResult {
    pub fn allow(source: PermissionSource) -> Self {
        Self {
            behavior: PermissionBehavior::Allow,
            source,
            reason: None,
            suggestions: Vec::new(),
            updated_input: None,
            decision_reason: None,
            interrupt: false,
            message: None,
        }
    }

    pub fn deny(source: PermissionSource, reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self {
            behavior: PermissionBehavior::Deny,
            source,
            reason: Some(reason.clone()),
            suggestions: Vec::new(),
            updated_input: None,
            decision_reason: None,
            interrupt: false,
            message: Some(reason),
        }
    }

    pub fn ask() -> Self {
        Self {
            behavior: PermissionBehavior::Ask,
            source: PermissionSource::ModeDefault,
            reason: None,
            suggestions: Vec::new(),
            updated_input: None,
            decision_reason: None,
            interrupt: false,
            message: None,
        }
    }

    /// Fluent helper — attach structured decision-reason metadata.
    pub fn with_decision_reason(mut self, reason: PermissionDecisionReason) -> Self {
        self.decision_reason = Some(reason);
        self
    }

    /// Fluent helper — attach "Allow always / Deny always" suggestions.
    pub fn with_suggestions(mut self, updates: Vec<PermissionUpdate>) -> Self {
        self.suggestions = updates;
        self
    }

    /// Fluent helper — carry a tool-rewritten input forward.
    pub fn with_updated_input(mut self, input: Value) -> Self {
        self.updated_input = Some(input);
        self
    }

    /// Fluent helper — mark the decision as bypass-immune / interrupt-
    /// raising. Used by SafetyCheck and `interrupt:true` hook returns.
    pub fn interrupting(mut self) -> Self {
        self.interrupt = true;
        self
    }

    /// Fluent helper — override the tool_result message shown to the model.
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }
}

/// Permission mode — mirrors the 6 TS `PermissionMode` values. The
/// engine maps each variant to a concrete behavior; today new variants
/// (`AcceptEdits`, `BypassPermissions`, `Auto`) fall through to the
/// same behavior as `DontAsk` / `Default` until Change B (tool
/// `check_permissions`) and Change E (classifier) wire them in.
///
/// This enum lived in `cc-permissions`; it moved to `cc-core` in the
/// D-A type extension so both `cc-permissions` and `PermissionDecisionReason`
/// can reference it without a circular dep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PermissionMode {
    /// Normal interactive mode: ask the user when no rule matches.
    #[default]
    Default,
    /// `acceptEdits`: auto-allow Edit/Write tools, still ask for Bash
    /// and other mutating tools. Pending Change B; today maps to
    /// `BypassPermissions` behavior.
    AcceptEdits,
    /// `bypassPermissions`: ignore the prompt entirely (deny rules +
    /// SafetyCheck still apply after Change B). Today auto-allows.
    BypassPermissions,
    /// `dontAsk`: per TS, turns any `ask` result into `deny` with a
    /// rejection message. Today maps to auto-allow; Change D will
    /// flip the semantics and add the rejection-message path.
    DontAsk,
    /// `plan` mode: read-only tools allowed; write tools auto-denied.
    Plan,
    /// `auto` mode: classifier-driven decisions. Pending Change E;
    /// today maps to `Default` (ask) as a safe fallback.
    Auto,
}

impl PermissionMode {
    /// Parse from the `defaultMode` settings string. Maps each TS
    /// value to its matching variant; unknown strings fall back to
    /// `Default`.
    pub fn from_settings_str(s: &str) -> Self {
        match s {
            "acceptEdits" => Self::AcceptEdits,
            "bypassPermissions" => Self::BypassPermissions,
            "dontAsk" => Self::DontAsk,
            "plan" => Self::Plan,
            "auto" => Self::Auto,
            _ => Self::Default,
        }
    }
}

/// A parsed permission rule (from settings.json allow/deny lists).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Abort the entire turn (not just this tool). Raised when a hook
    /// signals `interrupt: true` — e.g. a prompt-injection detector.
    /// Distinct from Ctrl+C (user-initiated) so the engine knows
    /// whether to surface a hook-produced message or the ordinary
    /// `[Request interrupted by user]` marker.
    Interrupt,
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

    /// Ask the user a free-form question and return their response.
    ///
    /// `options` is an optional list of suggested responses to display.
    /// The default implementation returns a message indicating the user
    /// is not available (non-interactive context).
    async fn ask_question(
        &self,
        question: &str,
        options: &[String],
        cancel: &CancellationToken,
    ) -> CcResult<String> {
        let _ = (question, options, cancel);
        Ok("User is not available to answer questions in this mode.".to_string())
    }
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
        assert!(allow.suggestions.is_empty());
        assert!(!allow.interrupt);

        let deny = PermissionResult::deny(PermissionSource::SettingsDeny, "blocked");
        assert_eq!(deny.behavior, PermissionBehavior::Deny);
        assert_eq!(deny.reason, Some("blocked".to_string()));
        assert_eq!(deny.message, Some("blocked".to_string()));
    }

    #[test]
    fn permission_result_fluent_builders() {
        let reason = PermissionDecisionReason::SafetyCheck {
            path: "/etc/passwd".into(),
        };
        let r = PermissionResult::deny(PermissionSource::SafetyCheck, "blocked")
            .with_decision_reason(reason.clone())
            .with_suggestions(vec![PermissionUpdate {
                destination: PermissionDestination::Session,
                rules: vec![PermissionRule::Simple("Bash(rm:*)".into())],
                behavior: PermissionBehavior::Deny,
            }])
            .with_updated_input(serde_json::json!({"redacted": true}))
            .with_message("please don't")
            .interrupting();

        assert_eq!(r.decision_reason, Some(reason));
        assert_eq!(r.suggestions.len(), 1);
        assert!(r.updated_input.is_some());
        assert_eq!(r.message, Some("please don't".into()));
        assert!(r.interrupt);
    }

    #[test]
    fn permission_mode_covers_all_ts_values() {
        // Each TS `defaultMode` string round-trips to a distinct variant.
        assert_eq!(
            PermissionMode::from_settings_str("acceptEdits"),
            PermissionMode::AcceptEdits
        );
        assert_eq!(
            PermissionMode::from_settings_str("bypassPermissions"),
            PermissionMode::BypassPermissions
        );
        assert_eq!(
            PermissionMode::from_settings_str("dontAsk"),
            PermissionMode::DontAsk
        );
        assert_eq!(
            PermissionMode::from_settings_str("plan"),
            PermissionMode::Plan
        );
        assert_eq!(
            PermissionMode::from_settings_str("auto"),
            PermissionMode::Auto
        );
        assert_eq!(
            PermissionMode::from_settings_str("bogus"),
            PermissionMode::Default
        );
    }

    #[test]
    fn prompt_decision_has_interrupt_variant() {
        // Anchor for Change D: hooks that set `interrupt:true` produce
        // `PromptDecision::Interrupt`, which the engine maps to a
        // turn-abort (vs Ctrl+C which bubbles as CcError::Cancelled).
        let d = PromptDecision::Interrupt;
        assert!(matches!(d, PromptDecision::Interrupt));
    }
}
