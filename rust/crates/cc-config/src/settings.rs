use cc_core::CcError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

use crate::merge::merge_json;
use crate::paths::ConfigPaths;

/// Permission rules section of settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PermissionsSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deny: Option<Vec<String>>,
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
    pub permissions: Option<PermissionsSettings>,

    /// Environment variables to inject.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,

    /// API key helper command.
    #[serde(skip_serializing_if = "Option::is_none", rename = "apiKeyHelper")]
    pub api_key_helper: Option<String>,

    /// All remaining unknown fields (preserved, not dropped).
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

impl Settings {
    /// Merge `other` on top of `self` (other wins on conflicts).
    /// Values from `other` override corresponding values in `self`.
    pub fn merge(self, other: Settings) -> Settings {
        // Use JSON merge so unknown fields in `extra` are preserved.
        let base = serde_json::to_value(self).unwrap_or(Value::Object(Default::default()));
        let overlay = serde_json::to_value(other).unwrap_or(Value::Object(Default::default()));
        let merged = merge_json(base, overlay);
        serde_json::from_value(merged).unwrap_or_default()
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

/// Expand short model aliases (e.g. `opus`, `sonnet`, `haiku`) to fully-qualified
/// model IDs accepted by the Anthropic API. Unknown inputs are returned as-is
/// so fully-qualified IDs pass through unchanged.
///
/// Matches the TS Claude Code CLI's alias behavior — users commonly write
/// `"model": "opus"` in `~/.claude/settings.json` and expect it to resolve to
/// the latest Opus ID.
pub fn expand_model_alias(input: &str) -> String {
    use cc_core::models;
    match input {
        "opus" | "claude-opus" => models::CLAUDE_OPUS_4_6.to_string(),
        "sonnet" | "claude-sonnet" | "default" => models::CLAUDE_SONNET_4_6.to_string(),
        "haiku" | "claude-haiku" => models::CLAUDE_HAIKU_4_5.to_string(),
        other => other.to_string(),
    }
}

/// Resolve the effective model for a session, applying the precedence:
///   1. `--model` CLI override (if `Some`)
///   2. `model` from merged settings (if `Some`)
///   3. compiled-in `models::DEFAULT`
///
/// After selecting a source, short aliases are expanded to full model IDs.
///
/// Pulled out of `main.rs` so it can be unit-tested without spawning a binary.
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

/// Load and merge settings in priority order (lowest → highest):
///   1. global (`~/.claude/settings.json`)
///   2. project (`.claude/settings.json`) — sanitized
///   3. local (`.claude/settings.local.json`)
///   4. CLI/SDK override (if provided)
///
/// `cwd` defaults to the current working directory if `None`.
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

    let global = load_file(&ConfigPaths::global_settings())?.unwrap_or_default();
    let project = load_file(&ConfigPaths::project_settings(cwd))?.unwrap_or_default();
    let project = sanitize_project_settings(project);
    let local = load_file(&ConfigPaths::local_settings(cwd))?.unwrap_or_default();

    let mut merged = global.merge(project).merge(local);

    // Layer 4: CLI/SDK override file
    if let Some(override_path) = cli_override {
        if let Some(override_settings) = load_file(override_path)? {
            merged = merged.merge(override_settings);
        }
    }

    Ok(merged)
}

/// Resolved configuration — combines project context + merged settings.
#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    pub project: crate::project::ProjectContext,
    pub model: String,
    pub max_tokens: u32,
    pub settings: Settings,
}

impl ResolvedConfig {
    /// Build a fully resolved config from CLI args and discovered project.
    pub fn resolve(
        project: crate::project::ProjectContext,
        cli_model: Option<&str>,
        cli_max_tokens: u32,
        cli_settings_path: Option<&Path>,
    ) -> Result<Self, CcError> {
        let settings = load_settings_with_override(
            Some(&project.original_cwd),
            cli_settings_path,
        )?;
        let model = resolve_model(cli_model, &settings);
        Ok(Self {
            project,
            model,
            max_tokens: cli_max_tokens,
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
        // 1. CLI wins over settings.
        let s = Settings {
            model: Some("claude-from-settings".into()),
            ..Default::default()
        };
        let m = resolve_model(Some("claude-cli-override"), &s);
        assert_eq!(m, "claude-cli-override");

        // 2. Settings wins when CLI is None.
        let m = resolve_model(None, &s);
        assert_eq!(m, "claude-from-settings");

        // 3. Default kicks in when both are absent / empty.
        let s = Settings::default();
        let m = resolve_model(None, &s);
        assert_eq!(m, cc_core::models::DEFAULT);

        // 4. Empty CLI string is treated as "not set".
        let m = resolve_model(Some(""), &s);
        assert_eq!(m, cc_core::models::DEFAULT);
    }

    #[test]
    fn expand_model_alias_maps_short_names() {
        assert_eq!(expand_model_alias("opus"), cc_core::models::CLAUDE_OPUS_4_6);
        assert_eq!(expand_model_alias("sonnet"), cc_core::models::CLAUDE_SONNET_4_6);
        assert_eq!(expand_model_alias("haiku"), cc_core::models::CLAUDE_HAIKU_4_5);
        assert_eq!(expand_model_alias("default"), cc_core::models::CLAUDE_SONNET_4_6);
        // Unknown / already-qualified values pass through.
        assert_eq!(expand_model_alias("claude-opus-4-6"), "claude-opus-4-6");
        assert_eq!(expand_model_alias("custom-finetune"), "custom-finetune");
    }

    #[test]
    fn resolve_model_expands_alias_from_settings() {
        // Regression: settings.json with `"model": "opus"` must be expanded
        // before being sent to the API — the raw alias is not a valid ID.
        let s = Settings {
            model: Some("opus".into()),
            ..Default::default()
        };
        assert_eq!(resolve_model(None, &s), cc_core::models::CLAUDE_OPUS_4_6);

        // CLI alias also expands.
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
}
