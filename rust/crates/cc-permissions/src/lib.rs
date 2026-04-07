use cc_core::PermissionBehavior;
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
    // Delegate to the glob crate for pattern matching.
    let pat = pattern.to_ascii_lowercase();
    let val = value.to_ascii_lowercase();
    glob::Pattern::new(&pat)
        .map(|p| p.matches(&val))
        .unwrap_or(false)
}

/// Engine that evaluates permission rules against tool invocations.
#[derive(Debug, Clone, Default)]
pub struct PermissionEngine {
    /// Rules that explicitly allow a tool (checked after deny).
    allow_rules: Vec<PermissionRule>,
    /// Rules that explicitly deny a tool (highest priority).
    deny_rules: Vec<PermissionRule>,
    /// Session-level allow rules added at runtime (e.g., user chose "always").
    session_allow: Vec<PermissionRule>,
}

impl PermissionEngine {
    /// Build a `PermissionEngine` from settings allow/deny lists.
    pub fn from_settings(
        allow: impl IntoIterator<Item = impl Into<String>>,
        deny: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        PermissionEngine {
            allow_rules: allow.into_iter().map(PermissionRule::new).collect(),
            deny_rules: deny.into_iter().map(PermissionRule::new).collect(),
            session_allow: Vec::new(),
        }
    }

    /// Add a session-level allow rule (user chose "always allow" at runtime).
    pub fn add_session_allow(&mut self, tool_name: impl Into<String>) {
        self.session_allow.push(PermissionRule::new(tool_name));
    }

    /// Evaluate the permission for a tool call.
    ///
    /// Order: deny → session allow → settings allow → Ask
    pub fn check(&self, tool_name: &str, _input: &Value) -> PermissionBehavior {
        // 1. Deny rules (highest priority)
        for rule in &self.deny_rules {
            if rule.matches_tool(tool_name) {
                return PermissionBehavior::Deny;
            }
        }
        // 2. Session-level allows (user approved this session)
        for rule in &self.session_allow {
            if rule.matches_tool(tool_name) {
                return PermissionBehavior::Allow;
            }
        }
        // 3. Settings allow rules
        for rule in &self.allow_rules {
            if rule.matches_tool(tool_name) {
                return PermissionBehavior::Allow;
            }
        }
        // 4. Default: ask the user
        PermissionBehavior::Ask
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cc_core::PermissionBehavior;
    use serde_json::json;

    #[test]
    fn deny_takes_priority_over_allow() {
        let engine = PermissionEngine::from_settings(["Bash"], ["Bash"]);
        assert!(matches!(engine.check("Bash", &json!({})), PermissionBehavior::Deny));
    }

    #[test]
    fn allow_rule_exact() {
        let engine = PermissionEngine::from_settings(["Read"], Vec::<String>::new());
        assert!(matches!(engine.check("Read", &json!({})), PermissionBehavior::Allow));
        assert!(matches!(engine.check("Write", &json!({})), PermissionBehavior::Ask));
    }

    #[test]
    fn wildcard_matches_all() {
        let engine = PermissionEngine::from_settings(["*"], Vec::<String>::new());
        assert!(matches!(engine.check("Bash", &json!({})), PermissionBehavior::Allow));
        assert!(matches!(engine.check("Write", &json!({})), PermissionBehavior::Allow));
    }

    #[test]
    fn session_allow_added_at_runtime() {
        let mut engine = PermissionEngine::default();
        assert!(matches!(engine.check("Bash", &json!({})), PermissionBehavior::Ask));
        engine.add_session_allow("Bash");
        assert!(matches!(engine.check("Bash", &json!({})), PermissionBehavior::Allow));
    }

    #[test]
    fn strip_input_specifier() {
        let rule = PermissionRule::new("Bash(*)");
        assert!(rule.matches_tool("Bash"));
    }
}
