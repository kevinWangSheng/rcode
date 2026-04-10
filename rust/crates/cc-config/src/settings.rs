use cc_core::hook::HooksSettings;
use cc_core::CcError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

use crate::merge::merge_json;
use crate::paths::ConfigPaths;

/// Permission rules section of settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PermissionsConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deny: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ask: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "defaultMode")]
    pub default_mode: Option<String>,
    /// Additional fields (unknown fields preserved per compatibility contract).
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

/// Top-level settings structure.
/// Unknown fields are preserved via `extra` to satisfy the compatibility contract:
/// "Unknown/invalid fields must be preserved (not silently dropped)".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Settings {
    /// Default model override.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// Permission rules.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permissions: Option<PermissionsConfig>,

    /// Hook definitions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hooks: Option<HooksSettings>,

    /// Environment variables to inject.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,

    /// MCP server configurations.
    #[serde(skip_serializing_if = "Option::is_none", rename = "mcpServers")]
    pub mcp_servers: Option<HashMap<String, Value>>,

    /// API key helper command.
    #[serde(skip_serializing_if = "Option::is_none", rename = "apiKeyHelper")]
    pub api_key_helper: Option<String>,

    /// All remaining unknown fields (preserved, not dropped).
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

impl Settings {
    /// Merge `other` on top of `self` (other wins on conflicts).
    pub fn merge(self, other: Settings) -> Settings {
        let base = serde_json::to_value(self).unwrap_or(Value::Object(Default::default()));
        let overlay = serde_json::to_value(other).unwrap_or(Value::Object(Default::default()));
        let merged = merge_json(base, overlay);
        serde_json::from_value(merged).unwrap_or_default()
    }
}

/// All settings sources, in merge priority order (lowest → highest).
#[derive(Debug, Default)]
pub struct SettingsSources {
    /// 1. Plugin base settings (allowlisted keys only).
    pub plugin_base: Option<Settings>,
    /// 2. User settings (`~/.claude/settings.json`).
    pub user: Option<Settings>,
    /// 3. Project settings (`.claude/settings.json`) — sanitized.
    pub project: Option<Settings>,
    /// 4. Local settings (`.claude/settings.local.json`).
    pub local: Option<Settings>,
    /// 5. CLI/SDK override (`--settings` flag).
    pub flag: Option<Settings>,
    /// 6. Managed/policy settings (first-source-wins for policy keys).
    pub policy: Option<Settings>,
}

/// Which settings sources are enabled (used by memory loader for gating).
#[derive(Debug, Clone)]
pub struct SettingsSourcesEnabled {
    pub user: bool,
    pub project: bool,
    pub local: bool,
}

impl Default for SettingsSourcesEnabled {
    fn default() -> Self {
        Self {
            user: true,
            project: true,
            local: true,
        }
    }
}

/// Load a settings file from disk, returning `None` if missing.
fn load_file(path: &Path) -> Result<Option<Settings>, CcError> {
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(path)?;
    let settings: Settings = serde_json::from_str(&content)?;
    Ok(Some(settings))
}

/// Expand short model aliases to fully-qualified model IDs.
pub fn expand_model_alias(input: &str) -> String {
    use cc_core::models;
    match input {
        "opus" | "claude-opus" => models::CLAUDE_OPUS_4_6.to_string(),
        "sonnet" | "claude-sonnet" | "default" => models::CLAUDE_SONNET_4_6.to_string(),
        "haiku" | "claude-haiku" => models::CLAUDE_HAIKU_4_5.to_string(),
        other => other.to_string(),
    }
}

/// Resolve the effective model: CLI > settings > default.
pub fn resolve_model(cli_model: Option<&str>, settings: &Settings) -> String {
    if let Some(m) = cli_model {
        if !m.is_empty() {
            return expand_model_alias(m);
        }
    }
    if let Some(m) = settings.model.as_deref() {
        if !m.is_empty() {
            return expand_model_alias(m);
        }
    }
    cc_core::models::DEFAULT.to_string()
}

/// Dangerous keys that must be stripped from project settings (security).
const DANGEROUS_PROJECT_KEYS: &[&str] = &[
    "skipDangerousModePermissionPrompt",
    "skipAutoPermissionPrompt",
    "useAutoModeDuringPlan",
    "autoMode",
];

/// Strip dangerous keys from project-level settings.
fn sanitize_project_settings(mut settings: Settings) -> Settings {
    for key in DANGEROUS_PROJECT_KEYS {
        settings.extra.remove(*key);
    }
    settings
}

/// Discover all settings sources for a project.
pub fn discover_sources(
    project: &crate::project::ProjectContext,
    cli_settings_path: Option<&Path>,
) -> Result<SettingsSources, CcError> {
    let user = load_file(&ConfigPaths::global_settings())?;
    let project_settings = load_file(&ConfigPaths::project_settings(&project.canonical_root))?
        .map(sanitize_project_settings);
    let local = load_file(&ConfigPaths::local_settings(&project.canonical_root))?;
    let flag = match cli_settings_path {
        Some(path) => load_file(path)?,
        None => None,
    };

    Ok(SettingsSources {
        plugin_base: None, // Loaded by plugin system if present
        user,
        project: project_settings,
        local,
        flag,
        policy: None, // Loaded from managed policy path if present
    })
}

/// Merge all sources in priority order.
pub fn merge_sources(sources: SettingsSources) -> Settings {
    let mut merged = Settings::default();

    if let Some(s) = sources.plugin_base {
        merged = merged.merge(s);
    }
    if let Some(s) = sources.user {
        merged = merged.merge(s);
    }
    if let Some(s) = sources.project {
        merged = merged.merge(s);
    }
    if let Some(s) = sources.local {
        merged = merged.merge(s);
    }
    if let Some(s) = sources.flag {
        merged = merged.merge(s);
    }
    if let Some(s) = sources.policy {
        merged = merged.merge(s);
    }

    merged
}

/// Load and merge all settings for a project.
pub fn load_settings(cwd: Option<&Path>) -> Result<Settings, CcError> {
    load_settings_with_override(cwd, None)
}

/// Load settings with an optional CLI/SDK override layer.
pub fn load_settings_with_override(
    cwd: Option<&Path>,
    cli_override: Option<&Path>,
) -> Result<Settings, CcError> {
    let cwd_buf;
    let cwd = match cwd {
        Some(p) => p,
        None => {
            cwd_buf = std::env::current_dir()?;
            &cwd_buf
        }
    };

    let project = crate::project::ProjectContext::discover(cwd);
    let sources = discover_sources(&project, cli_override)?;
    Ok(merge_sources(sources))
}

/// Resolved configuration — combines project context + merged settings.
#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    pub project: crate::project::ProjectContext,
    pub model: String,
    pub max_tokens: u32,
    pub permissions: PermissionsConfig,
    pub hooks: HooksSettings,
    pub env: HashMap<String, String>,
    pub mcp_servers: HashMap<String, Value>,
    pub settings: Settings,
    /// Full merged JSON (unknown fields preserved for round-trip).
    pub raw: Value,
}

impl ResolvedConfig {
    /// Build a fully resolved config from CLI args and discovered project.
    pub fn resolve(
        project: crate::project::ProjectContext,
        cli_model: Option<&str>,
        cli_max_tokens: u32,
        cli_settings_path: Option<&Path>,
    ) -> Result<Self, CcError> {
        let sources = discover_sources(&project, cli_settings_path)?;
        let settings = merge_sources(sources);
        let model = resolve_model(cli_model, &settings);
        let raw =
            serde_json::to_value(&settings).unwrap_or(Value::Object(Default::default()));

        Ok(Self {
            project,
            model,
            max_tokens: cli_max_tokens,
            permissions: settings.permissions.clone().unwrap_or_default(),
            hooks: settings.hooks.clone().unwrap_or_default(),
            env: settings.env.clone().unwrap_or_default(),
            mcp_servers: settings.mcp_servers.clone().unwrap_or_default(),
            raw,
            settings,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_model_override() {
        let base = Settings {
            model: Some("claude-sonnet-4-6".into()),
            ..Default::default()
        };
        let overlay = Settings {
            model: Some("claude-opus-4-6".into()),
            ..Default::default()
        };
        let merged = base.merge(overlay);
        assert_eq!(merged.model.as_deref(), Some("claude-opus-4-6"));
    }

    #[test]
    fn resolve_model_precedence_cli_over_settings_over_default() {
        let s = Settings {
            model: Some("claude-from-settings".into()),
            ..Default::default()
        };
        let m = resolve_model(Some("claude-cli-override"), &s);
        assert_eq!(m, "claude-cli-override");

        let m = resolve_model(None, &s);
        assert_eq!(m, "claude-from-settings");

        let s = Settings::default();
        let m = resolve_model(None, &s);
        assert_eq!(m, cc_core::models::DEFAULT);

        let m = resolve_model(Some(""), &s);
        assert_eq!(m, cc_core::models::DEFAULT);
    }

    #[test]
    fn expand_model_alias_maps_short_names() {
        assert_eq!(expand_model_alias("opus"), cc_core::models::CLAUDE_OPUS_4_6);
        assert_eq!(
            expand_model_alias("sonnet"),
            cc_core::models::CLAUDE_SONNET_4_6
        );
        assert_eq!(
            expand_model_alias("haiku"),
            cc_core::models::CLAUDE_HAIKU_4_5
        );
        assert_eq!(
            expand_model_alias("default"),
            cc_core::models::CLAUDE_SONNET_4_6
        );
        assert_eq!(expand_model_alias("claude-opus-4-6"), "claude-opus-4-6");
        assert_eq!(expand_model_alias("custom-finetune"), "custom-finetune");
    }

    #[test]
    fn resolve_model_expands_alias_from_settings() {
        let s = Settings {
            model: Some("opus".into()),
            ..Default::default()
        };
        assert_eq!(resolve_model(None, &s), cc_core::models::CLAUDE_OPUS_4_6);
        assert_eq!(
            resolve_model(Some("sonnet"), &Settings::default()),
            cc_core::models::CLAUDE_SONNET_4_6
        );
    }

    #[test]
    fn unknown_fields_preserved() {
        let json = r#"{"model":"claude-sonnet-4-6","unknownFutureProp":42}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert!(s.extra.contains_key("unknownFutureProp"));
        let round_trip = serde_json::to_string(&s).unwrap();
        assert!(round_trip.contains("unknownFutureProp"));
    }

    #[test]
    fn merge_sources_6_layer() {
        let sources = SettingsSources {
            plugin_base: Some(Settings {
                model: Some("plugin-model".into()),
                ..Default::default()
            }),
            user: Some(Settings {
                model: Some("user-model".into()),
                ..Default::default()
            }),
            project: None,
            local: None,
            flag: None,
            policy: Some(Settings {
                model: Some("policy-model".into()),
                ..Default::default()
            }),
        };
        let merged = merge_sources(sources);
        // Policy is highest priority, so it wins.
        assert_eq!(merged.model.as_deref(), Some("policy-model"));
    }

    #[test]
    fn sanitize_strips_dangerous_keys() {
        let json = r#"{"model":"opus","autoMode":true,"skipDangerousModePermissionPrompt":true}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        let sanitized = sanitize_project_settings(s);
        assert!(!sanitized.extra.contains_key("autoMode"));
        assert!(!sanitized
            .extra
            .contains_key("skipDangerousModePermissionPrompt"));
        assert_eq!(sanitized.model.as_deref(), Some("opus"));
    }

    #[test]
    fn hooks_and_mcp_roundtrip() {
        let json = r#"{
            "hooks": {
                "PreToolUse": [{"matcher": "Bash", "hooks": [{"command": "echo hi"}]}]
            },
            "mcpServers": {
                "fs": {"command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem"]}
            }
        }"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert!(s.hooks.is_some());
        assert!(s.mcp_servers.is_some());
        assert_eq!(s.mcp_servers.as_ref().unwrap().len(), 1);
    }
}
