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

/// Load and merge settings in priority order (lowest → highest):
///   global (`~/.claude/settings.json`)
///   → project (`.claude/settings.json`)
///   → local (`.claude/settings.local.json`)
///
/// `cwd` defaults to the current working directory if `None`.
pub fn load_settings(cwd: Option<&Path>) -> Result<Settings, CcError> {
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
    let local = load_file(&ConfigPaths::local_settings(cwd))?.unwrap_or_default();

    Ok(global.merge(project).merge(local))
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
    fn unknown_fields_preserved() {
        let json = r#"{"model":"claude-sonnet-4-6","unknownFutureProp":42}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert!(s.extra.contains_key("unknownFutureProp"));
        let round_trip = serde_json::to_string(&s).unwrap();
        assert!(round_trip.contains("unknownFutureProp"));
    }
}
