//! Skills & plugin loading — merged from cc-skills + cc-plugins per Phase 2 Decision 3.
//!
//! A skill is a markdown file (with optional YAML frontmatter) invocable as a slash
//! command. Two on-disk layouts:
//!
//!   - `<root>/skills/<name>.md`           (flat)
//!   - `<root>/skills/<name>/SKILL.md`     (directory form, allows assets alongside)
//!
//! Plugins are directories under `<root>/plugins/<name>/` containing `plugin.json`
//! and optional `skills/` / `commands/` subdirectories.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::debug;

/// A skill definition (from ~/.claude/skills/*.md or plugin).
#[derive(Debug, Clone)]
pub struct SkillDef {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    pub content: String,
    /// Optional model override (informational).
    pub model: Option<String>,
    /// Whether this skill is user-invocable as a slash command.
    pub user_invocable: bool,
}

/// Where a skill was discovered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SkillSource {
    User,
    Project,
    Plugin,
}

/// Discover skills from `~/.claude/skills/`, `<config_dir>/.claude/skills/`,
/// and plugin directories. Project skills override user skills with the same name.
pub fn discover_skills(config_dir: &Path) -> Vec<SkillDef> {
    let mut out: Vec<SkillDef> = Vec::new();

    // 1. User-level skills
    if let Some(user_dir) = dirs::home_dir().map(|h| h.join(".claude").join("skills")) {
        out.extend(load_skills_from(&user_dir, SkillSource::User));
    }

    // 2. Project-level skills
    let project_skills = config_dir.join(".claude").join("skills");
    for s in load_skills_from(&project_skills, SkillSource::Project) {
        if let Some(pos) = out.iter().position(|x| x.name == s.name) {
            out[pos] = s;
        } else {
            out.push(s);
        }
    }

    // 3. Plugin-contributed skills
    let plugin_skills = load_plugin_skills(config_dir);
    for s in plugin_skills {
        if let Some(pos) = out.iter().position(|x| x.name == s.name) {
            out[pos] = s;
        } else {
            out.push(s);
        }
    }

    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Load skills from a single directory.
fn load_skills_from(dir: &Path, source: SkillSource) -> Vec<SkillDef> {
    if !dir.exists() {
        return Vec::new();
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            debug!("skills: failed to read {:?}: {e}", dir);
            return Vec::new();
        }
    };

    let mut skills = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();

        // Directory form: <name>/SKILL.md
        if path.is_dir() {
            let skill_md = path.join("SKILL.md");
            if skill_md.exists() {
                let default_name = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("skill")
                    .to_string();
                if let Some(s) = load_one(&skill_md, source, default_name) {
                    skills.push(s);
                }
            }
            continue;
        }

        // Flat form: <name>.md
        if path.extension().and_then(|e| e.to_str()) == Some("md") {
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("skill")
                .to_string();
            if let Some(s) = load_one(&path, source, stem) {
                skills.push(s);
            }
        }
    }

    skills
}

fn load_one(path: &Path, source: SkillSource, default_name: String) -> Option<SkillDef> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            debug!("skills: skipping {:?}: {e}", path);
            return None;
        }
    };

    Some(parse_skill(&content, path, source, default_name))
}

fn parse_skill(content: &str, path: &Path, _source: SkillSource, default_name: String) -> SkillDef {
    let (frontmatter, body) = split_frontmatter(content);

    let name = frontmatter
        .as_ref()
        .and_then(|fm| extract_yaml_field(fm, "name"))
        .map(|s| sanitize_name(&s))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| sanitize_name(&default_name));

    let description = frontmatter
        .as_ref()
        .and_then(|fm| extract_yaml_field(fm, "description"))
        .unwrap_or_default();

    let model = frontmatter
        .as_ref()
        .and_then(|fm| extract_yaml_field(fm, "model"));

    let user_invocable = frontmatter
        .as_ref()
        .and_then(|fm| extract_yaml_field(fm, "user_invocable"))
        .map(|v| v == "true")
        .unwrap_or(true);

    SkillDef {
        name,
        description,
        path: path.to_path_buf(),
        content: body.trim().to_string(),
        model,
        user_invocable,
    }
}

/// Split YAML frontmatter from the body.
fn split_frontmatter(content: &str) -> (Option<String>, String) {
    let rest = if let Some(r) = content.strip_prefix("---\n") {
        r
    } else if let Some(r) = content.strip_prefix("---\r\n") {
        r
    } else {
        return (None, content.to_string());
    };

    // Find closing ---
    let mut idx = 0;
    for line in rest.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed == "---" {
            let fm = &rest[..idx];
            let body = &rest[idx + line.len()..];
            return (Some(fm.to_string()), body.to_string());
        }
        idx += line.len();
    }

    (None, content.to_string())
}

fn extract_yaml_field(frontmatter: &str, field: &str) -> Option<String> {
    let prefix = format!("{field}:");
    for line in frontmatter.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(&prefix) {
            let val = rest.trim().trim_matches('"').trim_matches('\'').to_string();
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    None
}

/// Lower-case, replace whitespace with `-`, strip non-alphanumeric (except - and _).
fn sanitize_name(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if ch == '-' || ch == '_' {
            out.push(ch);
        } else if ch.is_whitespace() {
            out.push('-');
        }
    }
    out
}

// --- Plugin support ---

/// Plugin manifest from `plugin.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PluginManifest {
    #[serde(default)]
    name: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    description: String,
}

/// Load skills contributed by plugins from user and project directories.
fn load_plugin_skills(config_dir: &Path) -> Vec<SkillDef> {
    let mut skills = Vec::new();

    // User plugins
    if let Some(user_dir) = dirs::home_dir().map(|h| h.join(".claude").join("plugins")) {
        skills.extend(load_plugins_from(&user_dir));
    }

    // Project plugins
    let project_dir = config_dir.join(".claude").join("plugins");
    skills.extend(load_plugins_from(&project_dir));

    skills
}

fn load_plugins_from(parent: &Path) -> Vec<SkillDef> {
    if !parent.exists() {
        return Vec::new();
    }

    let entries = match std::fs::read_dir(parent) {
        Ok(e) => e,
        Err(e) => {
            debug!("plugins: failed to read {:?}: {e}", parent);
            return Vec::new();
        }
    };

    let mut skills = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let manifest_path = path.join("plugin.json");
        if !manifest_path.exists() {
            continue;
        }

        // Read and validate manifest (skip on error)
        let Ok(raw) = std::fs::read_to_string(&manifest_path) else {
            continue;
        };
        let Ok(_manifest) = serde_json::from_str::<PluginManifest>(&raw) else {
            debug!("plugins: bad manifest at {:?}", manifest_path);
            continue;
        };

        // Load skills from plugin's skills/ and commands/ directories
        let skills_dir = path.join("skills");
        skills.extend(load_skills_from(&skills_dir, SkillSource::Plugin));

        let commands_dir = path.join("commands");
        skills.extend(load_skills_from(&commands_dir, SkillSource::Plugin));
    }

    skills
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn parses_skill_with_frontmatter() {
        let md = "---\nname: review-pr\ndescription: Review a pull request\nmodel: claude-opus-4-6\n---\nReview PR carefully.\n";
        let skill = parse_skill(md, Path::new("/tmp/x.md"), SkillSource::User, "x".into());
        assert_eq!(skill.name, "review-pr");
        assert_eq!(skill.description, "Review a pull request");
        assert_eq!(skill.model.as_deref(), Some("claude-opus-4-6"));
        assert_eq!(skill.content, "Review PR carefully.");
    }

    #[test]
    fn parses_skill_without_frontmatter() {
        let md = "Just a body, no frontmatter.\n";
        let skill = parse_skill(
            md,
            Path::new("/tmp/foo.md"),
            SkillSource::User,
            "foo".into(),
        );
        assert_eq!(skill.name, "foo");
        assert_eq!(skill.description, "");
        assert_eq!(skill.content, "Just a body, no frontmatter.");
    }

    #[test]
    fn name_sanitization() {
        assert_eq!(sanitize_name("Review PR"), "review-pr");
        assert_eq!(sanitize_name("foo_bar"), "foo_bar");
        assert_eq!(sanitize_name("HELLO!world"), "helloworld");
    }

    #[test]
    fn loads_flat_and_directory_forms() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        // Flat form
        fs::write(
            root.join("flat.md"),
            "---\nname: flat\ndescription: f\n---\nbody-flat",
        )
        .unwrap();

        // Directory form
        let sub = root.join("nested");
        fs::create_dir_all(&sub).unwrap();
        fs::write(
            sub.join("SKILL.md"),
            "---\ndescription: n\n---\nbody-nested",
        )
        .unwrap();

        let mut skills = load_skills_from(root, SkillSource::Project);
        skills.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(skills.len(), 2);
        assert_eq!(skills[0].name, "flat");
        assert_eq!(skills[0].content, "body-flat");
        assert_eq!(skills[1].name, "nested");
        assert_eq!(skills[1].content, "body-nested");
    }

    #[test]
    fn missing_directory_returns_empty() {
        let skills = load_skills_from(Path::new("/definitely/not/here"), SkillSource::User);
        assert!(skills.is_empty());
    }

    #[test]
    fn plugin_skills_loaded() {
        let dir = tempdir().unwrap();
        let plugin_root = dir.path().join(".claude").join("plugins").join("alpha");
        let skills_dir = plugin_root.join("skills");
        fs::create_dir_all(&skills_dir).unwrap();
        fs::write(
            plugin_root.join("plugin.json"),
            r#"{"name":"alpha","version":"0.1.0","description":"d"}"#,
        )
        .unwrap();
        fs::write(
            skills_dir.join("hello.md"),
            "---\ndescription: hi\n---\nbody",
        )
        .unwrap();

        let skills = load_plugin_skills(dir.path());
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "hello");
    }
}
