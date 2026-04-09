//! cc-memory — CLAUDE.md loading, walk-up, memory files, skills, plugins.
//!
//! Absorbs cc-skills + cc-plugins per Phase 2 Decision 3.

mod skills;

pub use skills::{discover_skills, SkillDef};

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tracing::debug;

/// Memory type (determines loading priority and source gating).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryType {
    Managed, // 1. policy path
    User,    // 2. ~/.claude/
    Project, // 3. walk-up (root→CWD)
    Local,   // 4. walk-up CLAUDE.local.md
    AutoMem, // 5. auto-memory
    TeamMem, // 6. team memory
}

/// A loaded memory file.
#[derive(Debug, Clone)]
pub struct MemoryFile {
    pub path: PathBuf,
    pub memory_type: MemoryType,
    pub name: String,
    pub description: String,
    pub content: String,
    /// Frontmatter globs (for rules files).
    pub globs: Option<Vec<String>>,
    /// Path of file that @-included this one (None if top-level).
    pub parent: Option<PathBuf>,
}

/// Load all memory files from `~/.claude/memory/*.md` (user type).
pub fn load_memories() -> Vec<MemoryFile> {
    let dir = match memory_dir() {
        Some(d) => d,
        None => return Vec::new(),
    };

    if !dir.exists() {
        return Vec::new();
    }

    let mut memories = Vec::new();

    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) => {
            debug!("failed to read memory dir: {e}");
            return Vec::new();
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                debug!("failed to read memory file {:?}: {e}", path);
                continue;
            }
        };

        let mem = parse_memory_file(&content, &path, MemoryType::User);
        memories.push(mem);
    }

    memories
}

/// Load CLAUDE.md files by walking up from CWD to project root.
pub fn load_claudemd_walk_up(cwd: &Path, root: &Path) -> Vec<MemoryFile> {
    let dirs = walk_up_dirs(cwd, root);
    let mut files = Vec::new();
    let mut processed = HashSet::new();

    // Process in reverse (root first → CWD last = highest priority)
    for dir in dirs.iter().rev() {
        // Project-type: CLAUDE.md
        let claude_md = dir.join("CLAUDE.md");
        if claude_md.exists() {
            if let Ok(content) = std::fs::read_to_string(&claude_md) {
                let mem = parse_memory_file(&content, &claude_md, MemoryType::Project);
                // Resolve @-includes
                let included = resolve_includes(
                    &content,
                    dir,
                    MemoryType::Project,
                    &mut processed,
                );
                files.extend(included);
                files.push(mem);
            }
        }

        // Local-type: CLAUDE.local.md
        let local_md = dir.join("CLAUDE.local.md");
        if local_md.exists() {
            if let Ok(content) = std::fs::read_to_string(&local_md) {
                let mem = parse_memory_file(&content, &local_md, MemoryType::Local);
                files.push(mem);
            }
        }
    }

    files
}

/// Collect directories from CWD up to (and including) root.
fn walk_up_dirs(cwd: &Path, root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut current = cwd.to_path_buf();
    loop {
        dirs.push(current.clone());
        if current == root || !current.pop() {
            break;
        }
    }
    dirs
}

/// Resolve @-includes in a memory file's content.
fn resolve_includes(
    content: &str,
    file_dir: &Path,
    memory_type: MemoryType,
    processed: &mut HashSet<PathBuf>,
) -> Vec<MemoryFile> {
    let mut included = Vec::new();

    for line in content.lines() {
        if let Some(path_ref) = parse_include_directive(line) {
            let resolved = resolve_include_path(path_ref, file_dir);
            let Some(resolved) = resolved else {
                continue;
            };

            // Circular reference prevention
            let canonical = resolved.canonicalize().unwrap_or(resolved.clone());
            if !processed.insert(canonical) {
                continue;
            }

            // Supported extensions check
            if !is_supported_extension(&resolved) {
                continue;
            }

            if let Ok(text) = std::fs::read_to_string(&resolved) {
                included.push(MemoryFile {
                    path: resolved,
                    memory_type,
                    name: String::new(),
                    description: String::new(),
                    content: text,
                    globs: None,
                    parent: Some(file_dir.to_path_buf()),
                });
            }
        }
    }
    included
}

/// Parse @path, @./rel, @~/home, @/abs from a line.
fn parse_include_directive(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if trimmed.starts_with('@') && trimmed.len() > 1 {
        Some(&trimmed[1..])
    } else {
        None
    }
}

/// Resolve an include path relative to the including file's directory.
fn resolve_include_path(path_ref: &str, base_dir: &Path) -> Option<PathBuf> {
    if path_ref.starts_with("~/") {
        dirs::home_dir().map(|h| h.join(&path_ref[2..]))
    } else if path_ref.starts_with('/') {
        Some(PathBuf::from(path_ref))
    } else if path_ref.starts_with("./") {
        Some(base_dir.join(&path_ref[2..]))
    } else {
        Some(base_dir.join(path_ref))
    }
}

/// Check if file has a supported extension for @-includes.
fn is_supported_extension(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("md" | "txt" | "json" | "yaml" | "yml" | "toml")
    )
}

fn memory_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("memory"))
}

/// Parse a memory file, extracting YAML frontmatter if present.
fn parse_memory_file(content: &str, path: &Path, memory_type: MemoryType) -> MemoryFile {
    let file_stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();

    if content.starts_with("---") {
        if let Some(rest) = content.strip_prefix("---") {
            if let Some(end_idx) = rest.find("\n---") {
                let frontmatter = &rest[..end_idx];
                let body = rest[end_idx + 4..].trim_start_matches('\n').to_string();

                let name = extract_yaml_field(frontmatter, "name").unwrap_or(file_stem.clone());
                let description =
                    extract_yaml_field(frontmatter, "description").unwrap_or_default();
                let globs = extract_yaml_list(frontmatter, "globs");

                return MemoryFile {
                    path: path.to_path_buf(),
                    memory_type,
                    name,
                    description,
                    content: body,
                    globs,
                    parent: None,
                };
            }
        }
    }

    // No frontmatter — use filename as name, entire content as body
    MemoryFile {
        path: path.to_path_buf(),
        memory_type,
        name: file_stem,
        description: String::new(),
        content: content.to_string(),
        globs: None,
        parent: None,
    }
}

fn extract_yaml_field(frontmatter: &str, field: &str) -> Option<String> {
    for line in frontmatter.lines() {
        if let Some(rest) = line.strip_prefix(&format!("{field}:")) {
            let val = rest.trim().trim_matches('"').trim_matches('\'').to_string();
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    None
}

fn extract_yaml_list(frontmatter: &str, field: &str) -> Option<Vec<String>> {
    let prefix = format!("{field}:");
    let mut found = false;
    let mut items = Vec::new();

    for line in frontmatter.lines() {
        if line.starts_with(&prefix) {
            found = true;
            // Inline format: globs: [*.rs, *.ts]
            let rest = line[prefix.len()..].trim();
            if rest.starts_with('[') && rest.ends_with(']') {
                let inner = &rest[1..rest.len() - 1];
                return Some(
                    inner
                        .split(',')
                        .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
                        .filter(|s| !s.is_empty())
                        .collect(),
                );
            }
            continue;
        }
        if found {
            let trimmed = line.trim();
            if trimmed.starts_with("- ") {
                items.push(trimmed[2..].trim().to_string());
            } else {
                break;
            }
        }
    }

    if items.is_empty() {
        None
    } else {
        Some(items)
    }
}

/// Format loaded memories as a system prompt block.
pub fn memories_to_system_text(memories: &[MemoryFile]) -> Option<String> {
    if memories.is_empty() {
        return None;
    }

    let parts: Vec<String> = memories
        .iter()
        .map(|m| {
            if m.content.is_empty() {
                format!("## {}\n{}", m.name, m.description)
            } else {
                format!("## {}\n{}", m.name, m.content)
            }
        })
        .collect();

    Some(format!("# Memory\n\n{}", parts.join("\n\n")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_memory_with_full_frontmatter() {
        let content =
            "---\nname: pref\ndescription: prefers concise output\ntype: feedback\n---\nBe concise.\n";
        let mem = parse_memory_file(content, &PathBuf::from("/tmp/pref.md"), MemoryType::User);
        assert_eq!(mem.name, "pref");
        assert_eq!(mem.memory_type, MemoryType::User);
        assert_eq!(mem.description, "prefers concise output");
        assert_eq!(mem.content.trim(), "Be concise.");
    }

    #[test]
    fn parse_memory_without_frontmatter_uses_filename() {
        let mem = parse_memory_file("plain body", &PathBuf::from("/tmp/old.md"), MemoryType::User);
        assert_eq!(mem.name, "old");
        assert_eq!(mem.memory_type, MemoryType::User);
        assert_eq!(mem.content, "plain body");
    }

    #[test]
    fn memories_to_system_text_aggregates_blocks() {
        let memories = vec![
            MemoryFile {
                path: PathBuf::from("/tmp/a.md"),
                memory_type: MemoryType::User,
                name: "a".into(),
                description: "".into(),
                content: "first".into(),
                globs: None,
                parent: None,
            },
            MemoryFile {
                path: PathBuf::from("/tmp/b.md"),
                memory_type: MemoryType::Project,
                name: "b".into(),
                description: "".into(),
                content: "second".into(),
                globs: None,
                parent: None,
            },
        ];
        let text = memories_to_system_text(&memories).expect("some text");
        assert!(text.contains("# Memory"));
        assert!(text.contains("## a"));
        assert!(text.contains("first"));
        assert!(text.contains("## b"));
        assert!(text.contains("second"));
    }

    #[test]
    fn empty_memories_return_none() {
        assert!(memories_to_system_text(&[]).is_none());
    }

    #[test]
    fn include_directive_parsing() {
        assert_eq!(parse_include_directive("@./local.md"), Some("./local.md"));
        assert_eq!(parse_include_directive("@~/docs/rules.md"), Some("~/docs/rules.md"));
        assert_eq!(parse_include_directive("@/abs/path.md"), Some("/abs/path.md"));
        assert_eq!(parse_include_directive("not an include"), None);
        assert_eq!(parse_include_directive("@"), None);
    }

    #[test]
    fn supported_extensions() {
        assert!(is_supported_extension(Path::new("file.md")));
        assert!(is_supported_extension(Path::new("file.txt")));
        assert!(is_supported_extension(Path::new("file.json")));
        assert!(!is_supported_extension(Path::new("file.exe")));
        assert!(!is_supported_extension(Path::new("file.rs")));
    }

    #[test]
    fn walk_up_from_nested() {
        let dirs = walk_up_dirs(
            Path::new("/repo/src/deep"),
            Path::new("/repo"),
        );
        assert_eq!(dirs.len(), 3);
        assert_eq!(dirs[0], Path::new("/repo/src/deep"));
        assert_eq!(dirs[1], Path::new("/repo/src"));
        assert_eq!(dirs[2], Path::new("/repo"));
    }

    #[test]
    fn yaml_list_parsing() {
        let fm = "globs: [*.rs, *.ts]\nname: test";
        let globs = extract_yaml_list(fm, "globs").unwrap();
        assert_eq!(globs, vec!["*.rs", "*.ts"]);

        let fm2 = "globs:\n  - *.py\n  - *.go\nname: test";
        let globs2 = extract_yaml_list(fm2, "globs").unwrap();
        assert_eq!(globs2, vec!["*.py", "*.go"]);
    }
}
