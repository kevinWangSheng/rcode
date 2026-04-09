//! cc-skills — load user skills from `~/.claude/skills/` and project `.claude/skills/`.
//!
//! A skill is a markdown file (with optional YAML frontmatter) that defines a reusable
//! prompt invocable as a slash command. Two on-disk layouts are supported:
//!
//!   - `<root>/skills/<name>.md`           (flat)
//!   - `<root>/skills/<name>/SKILL.md`     (directory form, allows assets alongside)
//!
//! Frontmatter fields:
//!
//! ```yaml
//! ---
//! name: my-skill        # optional, defaults to file stem / dir name
//! description: ...      # one-line summary, surfaced in /help
//! model: claude-...     # optional model override (informational for now)
//! ---
//! ```
//!
//! The body is the prompt template that gets injected as the user turn when the skill
//! is invoked. A skill with empty body and only a description is still loadable, but
//! invoking it produces only the description as the user message.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::debug;

/// A loaded skill ready to be invoked as a slash command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    /// Slash-command name (e.g., `review-pr`). Always lowercase, no spaces.
    pub name: String,
    /// One-line description shown in `/help`.
    pub description: String,
    /// Optional model override (currently informational).
    pub model: Option<String>,
    /// Prompt body — injected as the user message when the skill is invoked.
    pub body: String,
    /// Origin of the skill — useful for diagnostics and conflict resolution.
    pub source: SkillSource,
    /// Path the skill was loaded from.
    pub path: PathBuf,
}

/// Where a skill was discovered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SkillSource {
    /// Loaded from `~/.claude/skills/`.
    User,
    /// Loaded from `<cwd>/.claude/skills/`.
    Project,
    /// Loaded from a plugin bundle.
    Plugin,
}

/// Errors that can occur while loading skills.
#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    #[error("io error reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Load all skills from the user directory `~/.claude/skills/` and the project
/// directory `<cwd>/.claude/skills/`. On conflict (same name), project wins over user.
///
/// I/O errors on individual files are logged and skipped — a single bad file should
/// not prevent the rest from loading. This matches the philosophy in `cc-memory`.
pub fn load_skills() -> Vec<Skill> {
    let mut out: Vec<Skill> = Vec::new();

    if let Some(user_dir) = user_skills_dir() {
        out.extend(load_skills_from(&user_dir, SkillSource::User));
    }

    if let Ok(cwd) = std::env::current_dir() {
        let project_dir = cwd.join(".claude").join("skills");
        let project = load_skills_from(&project_dir, SkillSource::Project);

        // Project skills override user skills with the same name.
        for s in project {
            if let Some(pos) = out.iter().position(|x| x.name == s.name) {
                out[pos] = s;
            } else {
                out.push(s);
            }
        }
    }

    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Path to the user-level skills directory (`~/.claude/skills`).
pub fn user_skills_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("skills"))
}

/// Load skills from an arbitrary directory. Public so plugins can reuse it.
pub fn load_skills_from(dir: &Path, source: SkillSource) -> Vec<Skill> {
    if !dir.exists() {
        return Vec::new();
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            debug!("cc-skills: failed to read {:?}: {e}", dir);
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
                if let Some(s) = load_one(&skill_md, source, dir_default_name(&path)) {
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

fn dir_default_name(dir: &Path) -> String {
    dir.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("skill")
        .to_string()
}

fn load_one(path: &Path, source: SkillSource, default_name: String) -> Option<Skill> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            debug!("cc-skills: skipping {:?}: {e}", path);
            return None;
        }
    };

    Some(parse_skill(&content, path, source, default_name))
}

/// Parse a skill markdown string. Public for unit testing and reuse.
pub fn parse_skill(
    content: &str,
    path: &Path,
    source: SkillSource,
    default_name: String,
) -> Skill {
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

    Skill {
        name,
        description,
        model,
        body: body.trim().to_string(),
        source,
        path: path.to_path_buf(),
    }
}

/// Split YAML frontmatter from the body. Returns `(frontmatter, body)`.
/// If no frontmatter is present, returns `(None, full_content)`.
fn split_frontmatter(content: &str) -> (Option<String>, String) {
    let rest = if let Some(r) = content.strip_prefix("---\n") {
        r
    } else if let Some(r) = content.strip_prefix("---\r\n") {
        r
    } else {
        return (None, content.to_string());
    };

    if let Some(fence) = find_closing_fence(rest) {
        let fm = &rest[..fence.start];
        let body = &rest[fence.after..];
        (Some(fm.to_string()), body.to_string())
    } else {
        (None, content.to_string())
    }
}

struct Fence {
    start: usize,
    after: usize,
}

fn find_closing_fence(s: &str) -> Option<Fence> {
    let mut idx = 0;
    for line in s.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed == "---" {
            return Some(Fence {
                start: idx,
                after: idx + line.len(),
            });
        }
        idx += line.len();
    }
    None
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

/// Lower-case, replace whitespace with `-`, strip anything that isn't a-z0-9-_.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn parses_skill_with_frontmatter() {
        let md = "---\nname: review-pr\ndescription: Review a pull request\nmodel: claude-opus-4-6\n---\nReview PR {{args}} carefully.\n";
        let skill = parse_skill(md, Path::new("/tmp/x.md"), SkillSource::User, "x".into());
        assert_eq!(skill.name, "review-pr");
        assert_eq!(skill.description, "Review a pull request");
        assert_eq!(skill.model.as_deref(), Some("claude-opus-4-6"));
        assert_eq!(skill.body, "Review PR {{args}} carefully.");
    }

    #[test]
    fn parses_skill_without_frontmatter() {
        let md = "Just a body, no frontmatter.\n";
        let skill = parse_skill(md, Path::new("/tmp/foo.md"), SkillSource::User, "foo".into());
        assert_eq!(skill.name, "foo");
        assert_eq!(skill.description, "");
        assert_eq!(skill.body, "Just a body, no frontmatter.");
    }

    #[test]
    fn name_falls_back_to_filename_when_frontmatter_missing_name() {
        let md = "---\ndescription: hi\n---\nbody";
        let skill = parse_skill(md, Path::new("/tmp/bar.md"), SkillSource::Project, "bar".into());
        assert_eq!(skill.name, "bar");
        assert_eq!(skill.description, "hi");
    }

    #[test]
    fn sanitize_lowercases_and_dashes_whitespace() {
        assert_eq!(sanitize_name("Review PR"), "review-pr");
        assert_eq!(sanitize_name("foo_bar"), "foo_bar");
        assert_eq!(sanitize_name("HELLO!world"), "helloworld");
    }

    #[test]
    fn loads_flat_and_directory_forms_from_disk() {
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
        assert_eq!(skills[0].body, "body-flat");
        assert_eq!(skills[1].name, "nested");
        assert_eq!(skills[1].body, "body-nested");
    }

    #[test]
    fn missing_directory_returns_empty_not_error() {
        let skills = load_skills_from(Path::new("/definitely/not/here/cc-skills"), SkillSource::User);
        assert!(skills.is_empty());
    }
}
