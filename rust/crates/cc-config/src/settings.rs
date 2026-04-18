use cc_core::CcError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
    ///
    /// `base_label` / `overlay_label` identify which files contributed each
    /// layer so a shape-mismatch error can tell the user which two files to
    /// reconcile. Use [`Settings::merge`] for the label-less convenience form.
    pub fn merge_labeled(
        self,
        other: Settings,
        base_label: &str,
        overlay_label: &str,
    ) -> Result<Settings, CcError> {
        // Use JSON merge so unknown fields in `extra` are preserved.
        let base = serde_json::to_value(self)?;
        let overlay = serde_json::to_value(other)?;
        let merged = merge_json(base, overlay, base_label, overlay_label)?;
        Ok(serde_json::from_value(merged)?)
    }

    /// Convenience merge with unlabelled layers. Shape-mismatch errors will
    /// say `<base>` / `<overlay>` — prefer [`Settings::merge_labeled`] when
    /// callers know the source file paths.
    pub fn merge(self, other: Settings) -> Result<Settings, CcError> {
        self.merge_labeled(other, "<base>", "<overlay>")
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

fn path_label(path: &Path) -> String {
    path.display().to_string()
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

    let global_path: PathBuf = ConfigPaths::global_settings();
    let project_path: PathBuf = ConfigPaths::project_settings(cwd);
    let local_path: PathBuf = ConfigPaths::local_settings(cwd);

    let global = load_file(&global_path)?.unwrap_or_default();
    let project = load_file(&project_path)?.unwrap_or_default();
    let local = load_file(&local_path)?.unwrap_or_default();

    let global_label = path_label(&global_path);
    let project_label = path_label(&project_path);
    let local_label = path_label(&local_path);

    let merged_global_project = global
        .merge_labeled(project, &global_label, &project_label)
        .map_err(|e| annotate_shape_error(e, &global_label, &project_label))?;
    let merged = merged_global_project
        .merge_labeled(local, "<merged>", &local_label)
        .map_err(|e| annotate_shape_error(e, &global_label, &local_label))?;
    Ok(merged)
}

/// Re-wrap a shape-mismatch error with a hint naming the two potentially
/// conflicting files, since at merge time the "base" may already be a merged
/// product of earlier layers.
fn annotate_shape_error(err: CcError, hint_a: &str, hint_b: &str) -> CcError {
    match err {
        CcError::Config(msg) => CcError::Config(format!(
            "{msg} (check {hint_a} and {hint_b})"
        )),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
        let merged = base.merge(overlay).unwrap();
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

    #[test]
    fn unknown_fields_preserved_through_merge() {
        // Both layers contribute disjoint unknown fields; both must survive.
        let base: Settings =
            serde_json::from_str(r#"{"unknownA": {"x": 1}}"#).unwrap();
        let overlay: Settings =
            serde_json::from_str(r#"{"unknownB": [1, 2]}"#).unwrap();
        let merged = base.merge(overlay).unwrap();
        assert_eq!(merged.extra.get("unknownA"), Some(&json!({"x": 1})));
        assert_eq!(merged.extra.get("unknownB"), Some(&json!([1, 2])));
    }

    #[test]
    fn unknown_field_type_flip_is_rejected() {
        // base has `a` as object, overlay has `a` as array — previously this
        // silently flipped. It must now error.
        let base: Settings =
            serde_json::from_str(r#"{"a": {"x": 1}}"#).unwrap();
        let overlay: Settings = serde_json::from_str(r#"{"a": [1, 2]}"#).unwrap();
        let err = base
            .merge_labeled(overlay, "user.json", "project.json")
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("'a'"), "missing field: {msg}");
        assert!(msg.contains("user.json"), "missing base label: {msg}");
        assert!(msg.contains("project.json"), "missing overlay label: {msg}");
    }

    #[test]
    fn scalar_same_type_replaces_through_settings() {
        let base: Settings =
            serde_json::from_str(r#"{"model": "x"}"#).unwrap();
        let overlay: Settings =
            serde_json::from_str(r#"{"model": "y"}"#).unwrap();
        let merged = base.merge(overlay).unwrap();
        assert_eq!(merged.model.as_deref(), Some("y"));
    }
}
