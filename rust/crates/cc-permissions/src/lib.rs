use cc_core::{PermissionResult, PermissionSource};
use serde_json::Value;

/// A single permission rule entry — may include a tool name and optional input glob.
/// Formats:
///   "Bash"            — matches any Bash call
///   "Bash(*)"         — same (wildcard input matches all)
///   "Write(**)"       — matches any Write call
///   "mcp__fs__*"      — glob on tool name
///   {"tool": "Bash", "input": {"command": "git *"}} — structured with input matching
#[derive(Debug, Clone)]
pub struct PermissionRule {
    /// Tool name pattern (may contain `*`).
    pub tool_pattern: String,
    /// Optional input pattern — if set, all specified fields must match.
    /// Each value is a glob pattern matched against the corresponding field in tool input.
    pub input_pattern: Option<Value>,
}

impl PermissionRule {
    pub fn new(raw: impl Into<String>) -> Self {
        let raw = raw.into();
        // Strip optional input specifier like "Bash(*)" → "Bash"
        let tool_pattern = raw
            .split_once('(')
            .map(|(name, _)| name.trim().to_string())
            .unwrap_or(raw);
        PermissionRule {
            tool_pattern,
            input_pattern: None,
        }
    }

    /// Parse from a JSON value: string → simple rule, object → structured rule.
    pub fn from_value(val: Value) -> Option<Self> {
        match val {
            Value::String(s) => Some(Self::new(s)),
            Value::Object(ref obj) => {
                let tool = obj.get("tool")?.as_str()?;
                let input = obj.get("input").cloned();
                Some(PermissionRule {
                    tool_pattern: tool.to_string(),
                    input_pattern: input,
                })
            }
            _ => None,
        }
    }

    /// Check whether this rule matches the given tool name and input.
    pub fn matches(&self, tool_name: &str, tool_input: &Value) -> bool {
        if !glob_match(&self.tool_pattern, tool_name) {
            return false;
        }
        // If no input pattern, tool name match is sufficient
        let Some(ref pattern) = self.input_pattern else {
            return true;
        };
        input_matches(pattern, tool_input)
    }

    /// Check whether this rule matches just the tool name (legacy compat).
    pub fn matches_tool(&self, tool_name: &str) -> bool {
        glob_match(&self.tool_pattern, tool_name)
    }
}

/// Check if a tool input matches an input pattern.
/// Pattern fields are glob-matched against corresponding input fields.
/// All pattern fields must match for the overall match to succeed.
fn input_matches(pattern: &Value, input: &Value) -> bool {
    match (pattern, input) {
        (Value::Object(pat), Value::Object(inp)) => {
            for (key, pat_val) in pat {
                let Some(inp_val) = inp.get(key) else {
                    return false;
                };
                match pat_val {
                    Value::String(pat_str) => {
                        let inp_str = match inp_val {
                            Value::String(s) => s.as_str(),
                            _ => return false,
                        };
                        if !glob_match(pat_str, inp_str) {
                            return false;
                        }
                    }
                    _ => {
                        if !input_matches(pat_val, inp_val) {
                            return false;
                        }
                    }
                }
            }
            true
        }
        _ => pattern == input,
    }
}

/// Simple glob matching: supports `*` (matches any segment).
fn glob_match(pattern: &str, value: &str) -> bool {
    if pattern == "*" || pattern == "**" {
        return true;
    }
    if !pattern.contains('*') {
        return pattern.eq_ignore_ascii_case(value);
    }
    let pat = pattern.to_ascii_lowercase();
    let val = value.to_ascii_lowercase();
    glob::Pattern::new(&pat)
        .map(|p| p.matches(&val))
        .unwrap_or(false)
}

/// Permission mode — controls the default behavior when no rule matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PermissionMode {
    /// Normal interactive mode: ask the user when no rule matches.
    #[default]
    Default,
    /// `dontAsk` / auto-approve mode: auto-allow after deny check (no dialog).
    DontAsk,
    /// `plan` mode: read-only tools allowed; write tools auto-denied.
    Plan,
}

impl PermissionMode {
    /// Parse from the `defaultMode` settings string.
    pub fn from_settings_str(s: &str) -> Self {
        match s {
            "dontAsk" | "bypassPermissions" | "acceptEdits" => Self::DontAsk,
            "plan" => Self::Plan,
            _ => Self::Default,
        }
    }
}

/// Write tools — auto-denied in plan mode.
static WRITE_TOOLS: &[&str] = &["Write", "Edit", "Bash", "MultiEdit"];

fn is_write_tool(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    WRITE_TOOLS.iter().any(|w| lower == w.to_ascii_lowercase()) || lower.starts_with("mcp__")
    // MCP tools are assumed mutating
}

/// Engine that evaluates permission rules against tool invocations.
#[derive(Debug, Clone, Default)]
pub struct PermissionEngine {
    /// Rules that explicitly allow a tool.
    allow_rules: Vec<PermissionRule>,
    /// Rules that explicitly deny a tool (highest priority).
    deny_rules: Vec<PermissionRule>,
    /// Session-level allow rules added at runtime (e.g., user chose "always").
    session_allow: Vec<PermissionRule>,
    /// If true, all tools are allowed without prompting (--bypass-permissions flag).
    bypass: bool,
    /// Permission mode from settings defaultMode.
    mode: PermissionMode,
}

impl PermissionEngine {
    /// Build a `PermissionEngine` from settings allow/deny lists.
    pub fn from_settings(
        allow: impl IntoIterator<Item = impl Into<Value>>,
        deny: impl IntoIterator<Item = impl Into<Value>>,
    ) -> Self {
        PermissionEngine {
            allow_rules: allow
                .into_iter()
                .filter_map(|v| PermissionRule::from_value(v.into()))
                .collect(),
            deny_rules: deny
                .into_iter()
                .filter_map(|v| PermissionRule::from_value(v.into()))
                .collect(),
            session_allow: Vec::new(),
            bypass: false,
            mode: PermissionMode::Default,
        }
    }

    /// Enable bypass mode (--bypass-permissions flag).
    pub fn set_bypass(&mut self, bypass: bool) {
        self.bypass = bypass;
    }

    /// Set the permission mode from settings `defaultMode`.
    pub fn set_mode(&mut self, mode: PermissionMode) {
        self.mode = mode;
    }

    /// Set the permission mode from the string value in settings.
    pub fn set_mode_str(&mut self, mode: &str) {
        self.mode = PermissionMode::from_settings_str(mode);
    }

    /// Add a session-level allow rule (user chose "always allow" at runtime).
    pub fn add_session_allow(&mut self, tool_name: impl Into<String>) {
        self.session_allow.push(PermissionRule::new(tool_name));
    }

    /// Evaluate the permission for a tool call, returning the decision with audit trail.
    ///
    /// Priority: bypass → deny → session allow → settings allow → mode default → Ask
    pub fn check(&self, tool_name: &str, input: &Value) -> PermissionResult {
        // 0. Bypass mode (--bypass-permissions CLI flag)
        if self.bypass {
            return PermissionResult::allow(PermissionSource::BypassFlag);
        }

        // 1. Deny rules (highest priority among rule-based checks)
        for rule in &self.deny_rules {
            if rule.matches(tool_name, input) {
                return PermissionResult::deny(
                    PermissionSource::SettingsDeny,
                    format!("denied by rule: {}", rule.tool_pattern),
                );
            }
        }

        // 2. Session-level allows (user approved this session)
        for rule in &self.session_allow {
            if rule.matches(tool_name, input) {
                return PermissionResult::allow(PermissionSource::SessionAllow);
            }
        }

        // 3. Settings allow rules
        for rule in &self.allow_rules {
            if rule.matches(tool_name, input) {
                return PermissionResult::allow(PermissionSource::SettingsAllow);
            }
        }

        // 4. Mode-based default behavior
        match self.mode {
            PermissionMode::DontAsk => {
                // Auto-allow: no dialog, no prompt
                PermissionResult::allow(PermissionSource::ModeDefault)
            }
            PermissionMode::Plan => {
                if is_write_tool(tool_name) {
                    // Plan mode: write tools are auto-denied
                    PermissionResult::deny(
                        PermissionSource::ModeDefault,
                        format!("{tool_name} is not allowed in plan mode"),
                    )
                } else {
                    // Read-only tools allowed in plan mode
                    PermissionResult::allow(PermissionSource::ModeDefault)
                }
            }
            PermissionMode::Default => {
                // Ask the user interactively
                PermissionResult::ask()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cc_core::PermissionBehavior;
    use serde_json::json;

    #[test]
    fn deny_takes_priority_over_allow() {
        let engine = PermissionEngine::from_settings([json!("Bash")], [json!("Bash")]);
        let result = engine.check("Bash", &json!({}));
        assert_eq!(result.behavior, PermissionBehavior::Deny);
        assert_eq!(result.source, PermissionSource::SettingsDeny);
    }

    #[test]
    fn allow_rule_exact() {
        let engine = PermissionEngine::from_settings([json!("Read")], Vec::<Value>::new());
        let result = engine.check("Read", &json!({}));
        assert_eq!(result.behavior, PermissionBehavior::Allow);
        assert_eq!(result.source, PermissionSource::SettingsAllow);

        let result = engine.check("Write", &json!({}));
        assert_eq!(result.behavior, PermissionBehavior::Ask);
    }

    #[test]
    fn wildcard_matches_all() {
        let engine = PermissionEngine::from_settings([json!("*")], Vec::<Value>::new());
        assert_eq!(
            engine.check("Bash", &json!({})).behavior,
            PermissionBehavior::Allow
        );
        assert_eq!(
            engine.check("Write", &json!({})).behavior,
            PermissionBehavior::Allow
        );
    }

    #[test]
    fn session_allow_added_at_runtime() {
        let mut engine = PermissionEngine::default();
        assert_eq!(
            engine.check("Bash", &json!({})).behavior,
            PermissionBehavior::Ask
        );
        engine.add_session_allow("Bash");
        let result = engine.check("Bash", &json!({}));
        assert_eq!(result.behavior, PermissionBehavior::Allow);
        assert_eq!(result.source, PermissionSource::SessionAllow);
    }

    #[test]
    fn bypass_allows_everything() {
        let mut engine = PermissionEngine::from_settings(Vec::<Value>::new(), [json!("Bash")]);
        engine.set_bypass(true);
        let result = engine.check("Bash", &json!({}));
        assert_eq!(result.behavior, PermissionBehavior::Allow);
        assert_eq!(result.source, PermissionSource::BypassFlag);
    }

    #[test]
    fn strip_input_specifier() {
        let rule = PermissionRule::new("Bash(*)");
        assert!(rule.matches_tool("Bash"));
    }

    #[test]
    fn structured_rule_from_value() {
        let val = json!({"tool": "Bash", "input": {"command": "git *"}});
        let rule = PermissionRule::from_value(val).unwrap();
        assert!(rule.matches_tool("Bash"));
    }

    #[test]
    fn structured_rule_input_matching() {
        let val = json!({"tool": "Bash", "input": {"command": "git *"}});
        let rule = PermissionRule::from_value(val).unwrap();

        // Should match git commands
        assert!(rule.matches("Bash", &json!({"command": "git status"})));
        assert!(rule.matches("Bash", &json!({"command": "git push origin main"})));

        // Should NOT match non-git commands
        assert!(!rule.matches("Bash", &json!({"command": "rm -rf /"})));
        assert!(!rule.matches("Bash", &json!({"command": "ls -la"})));

        // Wrong tool name
        assert!(!rule.matches("Write", &json!({"command": "git status"})));
    }

    #[test]
    fn structured_rule_missing_input_field() {
        let val = json!({"tool": "Bash", "input": {"command": "git *"}});
        let rule = PermissionRule::from_value(val).unwrap();

        // Missing the "command" field entirely
        assert!(!rule.matches("Bash", &json!({})));
        assert!(!rule.matches("Bash", &json!({"other": "value"})));
    }

    #[test]
    fn structured_allow_in_engine() {
        let engine = PermissionEngine::from_settings(
            [json!({"tool": "Bash", "input": {"command": "git *"}})],
            Vec::<Value>::new(),
        );

        // git commands allowed
        let result = engine.check("Bash", &json!({"command": "git status"}));
        assert_eq!(result.behavior, PermissionBehavior::Allow);

        // non-git commands require asking
        let result = engine.check("Bash", &json!({"command": "rm -rf /"}));
        assert_eq!(result.behavior, PermissionBehavior::Ask);
    }

    #[test]
    fn mcp_tool_glob_pattern() {
        let engine = PermissionEngine::from_settings([json!("mcp__fs__*")], Vec::<Value>::new());
        assert_eq!(
            engine.check("mcp__fs__read_file", &json!({})).behavior,
            PermissionBehavior::Allow
        );
        assert_eq!(
            engine.check("mcp__fs__write_file", &json!({})).behavior,
            PermissionBehavior::Allow
        );
        assert_eq!(
            engine.check("mcp__other__tool", &json!({})).behavior,
            PermissionBehavior::Ask
        );
    }

    #[test]
    fn dont_ask_mode_auto_allows_after_deny() {
        let mut engine = PermissionEngine::default();
        engine.set_mode(PermissionMode::DontAsk);
        // No rules — mode should auto-allow
        let result = engine.check("Bash", &json!({}));
        assert_eq!(result.behavior, PermissionBehavior::Allow);
        assert_eq!(result.source, PermissionSource::ModeDefault);
    }

    #[test]
    fn dont_ask_mode_deny_still_applies() {
        let mut engine = PermissionEngine::from_settings(Vec::<Value>::new(), [json!("Bash")]);
        engine.set_mode(PermissionMode::DontAsk);
        // Deny rules still win even in dontAsk mode
        let result = engine.check("Bash", &json!({}));
        assert_eq!(result.behavior, PermissionBehavior::Deny);
        assert_eq!(result.source, PermissionSource::SettingsDeny);
    }

    #[test]
    fn plan_mode_denies_write_allows_read() {
        let mut engine = PermissionEngine::default();
        engine.set_mode(PermissionMode::Plan);

        // Write tools auto-denied
        assert_eq!(
            engine.check("Write", &json!({})).behavior,
            PermissionBehavior::Deny
        );
        assert_eq!(
            engine.check("Bash", &json!({})).behavior,
            PermissionBehavior::Deny
        );
        assert_eq!(
            engine.check("Edit", &json!({})).behavior,
            PermissionBehavior::Deny
        );

        // Read-only tools auto-allowed
        assert_eq!(
            engine.check("Read", &json!({})).behavior,
            PermissionBehavior::Allow
        );
        assert_eq!(
            engine.check("Glob", &json!({})).behavior,
            PermissionBehavior::Allow
        );
        assert_eq!(
            engine.check("Grep", &json!({})).behavior,
            PermissionBehavior::Allow
        );
    }

    #[test]
    fn plan_mode_deny_rule_still_applies_to_reads() {
        let mut engine = PermissionEngine::from_settings(Vec::<Value>::new(), [json!("Read")]);
        engine.set_mode(PermissionMode::Plan);
        // Explicit deny overrides plan-mode allow for read tools
        assert_eq!(
            engine.check("Read", &json!({})).behavior,
            PermissionBehavior::Deny
        );
    }

    #[test]
    fn set_mode_str_parses_known_values() {
        let mut engine = PermissionEngine::default();
        engine.set_mode_str("dontAsk");
        assert_eq!(engine.mode, PermissionMode::DontAsk);

        engine.set_mode_str("plan");
        assert_eq!(engine.mode, PermissionMode::Plan);

        engine.set_mode_str("unknown");
        assert_eq!(engine.mode, PermissionMode::Default);

        engine.set_mode_str("bypassPermissions");
        assert_eq!(engine.mode, PermissionMode::DontAsk);
    }
}
