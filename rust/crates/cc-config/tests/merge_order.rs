//! Settings merge precedence test (M4 exit criterion 7).
//!
//! Confirms the order claimed in `Section 3 / Compatibility Contracts`:
//!   global → project → local (lowest to highest priority).
//!
//! This duplicates what `load_settings` does internally so we can run it
//! without touching the user's real `~/.claude` directory.

use cc_config::Settings;
use serde_json::json;
use std::fs;
use tempfile::tempdir;

fn write_settings(path: &std::path::Path, body: serde_json::Value) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body.to_string()).unwrap();
}

fn load_one(path: &std::path::Path) -> Settings {
    let content = fs::read_to_string(path).unwrap();
    serde_json::from_str(&content).unwrap()
}

#[test]
fn local_overrides_project_overrides_global() {
    let dir = tempdir().unwrap();
    let global_path = dir.path().join("global/settings.json");
    let project_path = dir.path().join("proj/.claude/settings.json");
    let local_path = dir.path().join("proj/.claude/settings.local.json");

    write_settings(
        &global_path,
        json!({
            "model": "global-model",
            "permissions": {"allow": ["Bash(ls:*)"]},
            "extra_global_only": "g"
        }),
    );
    write_settings(
        &project_path,
        json!({
            "model": "project-model",
            "permissions": {"allow": ["Bash(grep:*)"]},
            "extra_project_only": "p"
        }),
    );
    write_settings(
        &local_path,
        json!({
            "model": "local-model",
            "extra_local_only": "l"
        }),
    );

    let global = load_one(&global_path);
    let project = load_one(&project_path);
    let local = load_one(&local_path);

    let merged = global.merge(project).merge(local);

    // Local model wins.
    assert_eq!(merged.model.as_deref(), Some("local-model"));

    // Permissions: local doesn't override permissions, so project's value wins
    // over global's. (`allow` is wholly replaced because it's a non-object Vec.)
    let allow = merged
        .permissions
        .as_ref()
        .and_then(|p| p.allow.as_ref())
        .cloned()
        .unwrap_or_default();
    assert_eq!(allow, vec!["Bash(grep:*)".to_string()]);

    // Unknown fields from each layer are preserved.
    assert_eq!(merged.extra.get("extra_global_only"), Some(&json!("g")));
    assert_eq!(merged.extra.get("extra_project_only"), Some(&json!("p")));
    assert_eq!(merged.extra.get("extra_local_only"), Some(&json!("l")));
}

#[test]
fn missing_layers_fall_through_to_lower_layer() {
    // Only global is present; merge should be a no-op for missing layers.
    let global = Settings {
        model: Some("only-global".into()),
        ..Default::default()
    };
    let merged = global.clone().merge(Settings::default()).merge(Settings::default());
    assert_eq!(merged.model.as_deref(), Some("only-global"));
}
