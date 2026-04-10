use cc_core::{PermissionResult, PermissionSource};
use serde_json::Value;

/// A single permission rule entry — may include a tool name and optional input glob.
/// Formats:
///   "Bash"            — matches any Bash call
///   "Bash(*)"         — same (wildcard input matches all)
///   "Write(**)"       — matches any Write call
///   "mcp__fs__*"      — glob on tool name
#[derive(Debug, Clone)]
pub struct PermissionRule {
    /// Tool name pattern (may contain `*`).
    pub tool_pattern: String,
}

impl PermissionRule {
    pub fn new(raw: impl Into<String>) -> Self {
        let raw = raw.into();
        // Strip optional input specifier like "Bash(*)" → "Bash"
        let tool_pattern = raw
            .split_once('(')
            .map(|(name, _)| name.trim().to_string())
            .unwrap_or(raw);
        PermissionRule { tool_pattern }
    }

    /// Parse from a JSON value: string → simple rule, object → structured rule.
    pub fn from_value(val: Value) -> Option<Self> {
        match val {
            Value::String(s) => Some(Self::new(s)),
            Value::Object(ref obj) => {
                // Structured rule: { "tool": "Bash", "input": {...} }
                let tool = obj.get("tool")?.as_str()?;
                Some(Self::new(tool))
            }
            _ => None,
        }
    }

    /// Check whether this rule matches the given tool name.
    pub fn matches_tool(&self, tool_name: &str) -> bool {
        glob_match(&self.tool_pattern, tool_name)
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

/// Engine that evaluates permission rules against tool invocations.
#[derive(Debug, Clone, Default)]
pub struct PermissionEngine {
    /// Rules that explicitly allow a tool.
    allow_rules: Vec<PermissionRule>,
    /// Rules that explicitly deny a tool (highest priority).
    deny_rules: Vec<PermissionRule>,
    /// Session-level allow rules added at runtime (e.g., user chose "always").
    session_allow: Vec<PermissionRule>,
    /// If true, all tools are allowed without prompting.
    bypass: bool,
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
        }
    }

    /// Enable bypass mode (--bypass-permissions flag).
    pub fn set_bypass(&mut self, bypass: bool) {
        self.bypass = bypass;
    }

    /// Add a session-level allow rule (user chose "always allow" at runtime).
    pub fn add_session_allow(&mut self, tool_name: impl Into<String>) {
        self.session_allow.push(PermissionRule::new(tool_name));
    }

    /// Evaluate the permission for a tool call, returning the decision with audit trail.
    ///
    /// Priority: bypass → deny → session allow → settings allow → Ask
    pub fn check(&self, tool_name: &str, _input: &Value) -> PermissionResult {
        // 0. Bypass mode
        if self.bypass {
            return PermissionResult::allow(PermissionSource::BypassFlag);
        }

        // 1. Deny rules (highest priority)
        for rule in &self.deny_rules {
            if rule.matches_tool(tool_name) {
                return PermissionResult::deny(
                    PermissionSource::SettingsDeny,
                    format!("denied by rule: {}", rule.tool_pattern),
                );
            }
        }

        // 2. Session-level allows (user approved this session)
        for rule in &self.session_allow {
            if rule.matches_tool(tool_name) {
                return PermissionResult::allow(PermissionSource::SessionAllow);
            }
        }

        // 3. Settings allow rules
        for rule in &self.allow_rules {
            if rule.matches_tool(tool_name) {
                return PermissionResult::allow(PermissionSource::SettingsAllow);
            }
        }

        // 4. Default: ask the user
        PermissionResult::ask()
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
}
