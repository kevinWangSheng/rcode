//! cc-plugins — discover and load plugin bundles.
//!
//! A plugin is a directory under `~/.claude/plugins/<name>/` (or project-level
//! `<cwd>/.claude/plugins/<name>/`) containing a `plugin.json` manifest and any of:
//!
//!   - `skills/`     — markdown skill files (loaded via `cc-skills`)
//!   - `commands/`   — `*.md` slash-command bodies (parsed as skills with source=Plugin)
//!
//! Manifest schema (`plugin.json`):
//!
//! ```json
//! {
//!   "name": "my-plugin",
//!   "version": "0.1.0",
//!   "description": "what this plugin does",
//!   "author": "optional"
//! }
//! ```
//!
//! Plugins are intentionally minimal at this milestone — they're a packaging mechanism
//! that bundles skills together. Code execution / WASM hooks are out of scope for M3.

use std::path::{Path, PathBuf};

use cc_skills::{load_skills_from, Skill, SkillSource};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

/// Manifest fields parsed from `plugin.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
}

/// A loaded plugin with its manifest, root directory, and the skills it contributes.
#[derive(Debug, Clone)]
pub struct Plugin {
    pub manifest: PluginManifest,
    pub root: PathBuf,
    pub skills: Vec<Skill>,
}

/// Errors that can occur while loading plugins.
#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("manifest at {path} is invalid: {reason}")]
    BadManifest { path: PathBuf, reason: String },
}

/// Load every plugin from `~/.claude/plugins/` and `<cwd>/.claude/plugins/`.
///
/// Project plugins override user plugins with the same `name` (last write wins).
pub fn load_plugins() -> Vec<Plugin> {
    let mut out: Vec<Plugin> = Vec::new();

    if let Some(user_dir) = user_plugins_dir() {
        out.extend(load_plugins_from(&user_dir));
    }

    if let Ok(cwd) = std::env::current_dir() {
        let project_dir = cwd.join(".claude").join("plugins");
        for p in load_plugins_from(&project_dir) {
            if let Some(pos) = out.iter().position(|x| x.manifest.name == p.manifest.name) {
                out[pos] = p;
            } else {
                out.push(p);
            }
        }
    }

    out.sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name));
    out
}

/// Path to the user-level plugins directory (`~/.claude/plugins`).
pub fn user_plugins_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("plugins"))
}

/// Load plugins from an arbitrary parent directory whose immediate children are
/// individual plugin directories.
pub fn load_plugins_from(parent: &Path) -> Vec<Plugin> {
    if !parent.exists() {
        return Vec::new();
    }

    let entries = match std::fs::read_dir(parent) {
        Ok(e) => e,
        Err(e) => {
            debug!("cc-plugins: failed to read {:?}: {e}", parent);
            return Vec::new();
        }
    };

    let mut plugins = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        match load_plugin(&path) {
            Ok(Some(p)) => plugins.push(p),
            Ok(None) => {} // No manifest — silently skip.
            Err(e) => warn!("cc-plugins: skipping {:?}: {e}", path),
        }
    }
    plugins
}

/// Load a single plugin from its root directory.
///
/// Returns `Ok(None)` if there is no `plugin.json` (treated as "not a plugin",
/// not an error). Returns `Err` only for malformed manifests we can see.
pub fn load_plugin(root: &Path) -> Result<Option<Plugin>, PluginError> {
    let manifest_path = root.join("plugin.json");
    if !manifest_path.exists() {
        return Ok(None);
    }

    let raw = std::fs::read_to_string(&manifest_path).map_err(|e| PluginError::BadManifest {
        path: manifest_path.clone(),
        reason: e.to_string(),
    })?;

    let mut manifest: PluginManifest =
        serde_json::from_str(&raw).map_err(|e| PluginError::BadManifest {
            path: manifest_path.clone(),
            reason: e.to_string(),
        })?;

    if manifest.name.trim().is_empty() {
        // Fall back to directory name so a missing field doesn't break loading.
        manifest.name = root
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
    }

    // Skills bundled in the plugin (both layouts: skills/ and commands/).
    let mut skills = Vec::new();
    let skills_dir = root.join("skills");
    skills.extend(load_skills_from(&skills_dir, SkillSource::Plugin));

    let commands_dir = root.join("commands");
    skills.extend(load_skills_from(&commands_dir, SkillSource::Plugin));

    Ok(Some(Plugin {
        manifest,
        root: root.to_path_buf(),
        skills,
    }))
}

/// Flatten skills from all plugins into a single vector. Useful for the command
/// registry which only cares about the flat list.
pub fn collect_skills(plugins: &[Plugin]) -> Vec<Skill> {
    plugins.iter().flat_map(|p| p.skills.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_plugin(parent: &Path, name: &str, with_skill: bool) {
        let root = parent.join(name);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("plugin.json"),
            format!(r#"{{"name":"{name}","version":"0.1.0","description":"d"}}"#),
        )
        .unwrap();

        if with_skill {
            let skills = root.join("skills");
            fs::create_dir_all(&skills).unwrap();
            fs::write(
                skills.join("hello.md"),
                "---\ndescription: hi\n---\nbody",
            )
            .unwrap();
        }
    }

    #[test]
    fn loads_plugin_with_manifest_and_skills() {
        let dir = tempdir().unwrap();
        write_plugin(dir.path(), "alpha", true);

        let plugins = load_plugins_from(dir.path());
        assert_eq!(plugins.len(), 1);
        let p = &plugins[0];
        assert_eq!(p.manifest.name, "alpha");
        assert_eq!(p.skills.len(), 1);
        assert_eq!(p.skills[0].name, "hello");
        assert_eq!(p.skills[0].source, SkillSource::Plugin);
    }

    #[test]
    fn skips_directories_without_manifest() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("not-a-plugin")).unwrap();

        let plugins = load_plugins_from(dir.path());
        assert!(plugins.is_empty());
    }

    #[test]
    fn bad_manifest_logs_and_skips_does_not_panic() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("broken");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("plugin.json"), "{ this is not json").unwrap();

        let plugins = load_plugins_from(dir.path());
        assert!(plugins.is_empty());
    }

    #[test]
    fn collect_skills_flattens() {
        let dir = tempdir().unwrap();
        write_plugin(dir.path(), "alpha", true);
        write_plugin(dir.path(), "beta", true);

        let plugins = load_plugins_from(dir.path());
        let skills = collect_skills(&plugins);
        assert_eq!(skills.len(), 2);
    }

    #[test]
    fn manifest_missing_name_falls_back_to_dirname() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("autonamed");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("plugin.json"), "{}").unwrap();

        let p = load_plugin(&root).unwrap().unwrap();
        assert_eq!(p.manifest.name, "autonamed");
    }
}
