//! cc-memory — CLAUDE.md loading, walk-up, memory files, skills, plugins.
//!
//! Absorbs cc-skills + cc-plugins per Phase 2 Decision 3.

mod skills;

pub use skills::{discover_skills, SkillDef};

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

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
                let included = resolve_includes(&content, dir, MemoryType::Project, &mut processed);
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

/// Stable dedup key for cycle detection of `@`-includes.
///
/// On Unix we prefer `(device, inode)` from `metadata()` — that catches
/// symlink cycles even when `canonicalize()` returns `Err` (e.g. permission
/// denied on a link target). When `metadata()` fails we fall back to a
/// lexically-normalized path key so the cycle detector still makes forward
/// progress instead of silently de-duping to the same un-canonicalized path
/// twice and entering an infinite recursion through a different syntactic
/// include form.
#[derive(Clone, Hash, PartialEq, Eq, Debug)]
enum IncludeKey {
    #[cfg(unix)]
    DevIno(u64, u64),
    Lexical(PathBuf),
}

fn include_dedup_key(path: &Path) -> IncludeKey {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let Ok(md) = std::fs::metadata(path) {
            return IncludeKey::DevIno(md.dev(), md.ino());
        }
    }
    IncludeKey::Lexical(lexical_normalize(path))
}

/// Lexical path normalization (no filesystem calls) used as the cycle-key
/// fallback when metadata/canonicalize fail.
fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out: Vec<std::path::Component<'_>> = Vec::new();
    for comp in path.components() {
        use std::path::Component::*;
        match comp {
            CurDir => {}
            ParentDir => match out.last() {
                Some(Normal(_)) => {
                    out.pop();
                }
                Some(RootDir) | Some(Prefix(_)) => {}
                _ => out.push(comp),
            },
            other => out.push(other),
        }
    }
    let mut buf = PathBuf::new();
    for c in out {
        buf.push(c.as_os_str());
    }
    buf
}

/// Resolve @-includes in a memory file's content.
fn resolve_includes(
    content: &str,
    file_dir: &Path,
    memory_type: MemoryType,
    processed: &mut HashSet<IncludeKey>,
) -> Vec<MemoryFile> {
    let mut included = Vec::new();

    for line in content.lines() {
        if let Some(path_ref) = parse_include_directive(line) {
            let Some(resolved) = resolve_include_path(path_ref, file_dir) else {
                warn!(
                    token = path_ref,
                    base = %file_dir.display(),
                    "cc-memory: @-include could not be resolved"
                );
                continue;
            };

            // Cycle detection: dev+ino on Unix, lexical fallback otherwise.
            // Using this instead of `canonicalize().unwrap_or(path)` means a
            // symlink with an unreadable target does NOT silently bypass
            // dedup and loop forever.
            let key = include_dedup_key(&resolved);
            if !processed.insert(key) {
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
    if let Some(rest) = path_ref.strip_prefix("~/") {
        dirs::home_dir().map(|h| h.join(rest))
    } else if path_ref.starts_with('/') {
        Some(PathBuf::from(path_ref))
    } else if let Some(rest) = path_ref.strip_prefix("./") {
        Some(base_dir.join(rest))
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
            if let Some(rest) = trimmed.strip_prefix("- ") {
                items.push(rest.trim().to_string());
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

/// Load all memory files for the current project (all 6 types).
pub fn load_memory_files(
    cwd: &Path,
    root: &Path,
    sources: &cc_config::SettingsSourcesEnabled,
) -> Vec<MemoryFile> {
    let mut files = Vec::new();

    // 1. Managed (policy path) — currently no managed policy path implemented
    // files.extend(load_managed_memory());

    // 2. User (~/.claude/memory/*.md)
    if sources.user {
        files.extend(load_memories());
    }

    // 3 + 4. Project (CLAUDE.md walk-up) + Local (CLAUDE.local.md walk-up)
    if sources.project || sources.local {
        files.extend(load_claudemd_walk_up(cwd, root));
    }

    // 5. AutoMem — loaded separately by the auto-memory system
    // 6. TeamMem — loaded separately by the team system

    files
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
        let mem = parse_memory_file(
            "plain body",
            &PathBuf::from("/tmp/old.md"),
            MemoryType::User,
        );
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
        assert_eq!(
            parse_include_directive("@~/docs/rules.md"),
            Some("~/docs/rules.md")
        );
        assert_eq!(
            parse_include_directive("@/abs/path.md"),
            Some("/abs/path.md")
        );
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
        let dirs = walk_up_dirs(Path::new("/repo/src/deep"), Path::new("/repo"));
        assert_eq!(dirs.len(), 3);
        assert_eq!(dirs[0], Path::new("/repo/src/deep"));
        assert_eq!(dirs[1], Path::new("/repo/src"));
        assert_eq!(dirs[2], Path::new("/repo"));
    }

    #[test]
    fn circular_include_does_not_loop() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();

        // a.md includes b.md, b.md includes a.md
        std::fs::write(root.join("a.md"), "@./b.md\nContent A").unwrap();
        std::fs::write(root.join("b.md"), "@./a.md\nContent B").unwrap();

        let mut processed = HashSet::new();
        let included = resolve_includes(
            &std::fs::read_to_string(root.join("a.md")).unwrap(),
            root,
            MemoryType::Project,
            &mut processed,
        );
        // Should include b.md but NOT re-include a.md (circular ref prevented)
        assert_eq!(included.len(), 1);
        assert!(included[0].content.contains("Content B"));
    }

    #[test]
    fn load_claudemd_walk_up_finds_all_levels() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        let nested = root.join("src").join("deep");
        std::fs::create_dir_all(&nested).unwrap();

        // CLAUDE.md at root
        std::fs::write(root.join("CLAUDE.md"), "Root instructions").unwrap();
        // CLAUDE.md at src/
        std::fs::write(root.join("src").join("CLAUDE.md"), "Src instructions").unwrap();
        // CLAUDE.local.md at src/deep/
        std::fs::write(nested.join("CLAUDE.local.md"), "Local overrides").unwrap();

        let files = load_claudemd_walk_up(&nested, root);
        assert!(files.len() >= 3);

        let contents: Vec<&str> = files.iter().map(|f| f.content.as_str()).collect();
        assert!(contents.iter().any(|c| c.contains("Root instructions")));
        assert!(contents.iter().any(|c| c.contains("Src instructions")));
        assert!(contents.iter().any(|c| c.contains("Local overrides")));
    }

    #[test]
    fn at_include_resolves_relative_path() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();

        std::fs::write(root.join("rules.md"), "Shared rules content").unwrap();
        std::fs::write(root.join("CLAUDE.md"), "@./rules.md\nMain content").unwrap();

        let mut processed = HashSet::new();
        let included = resolve_includes(
            &std::fs::read_to_string(root.join("CLAUDE.md")).unwrap(),
            root,
            MemoryType::Project,
            &mut processed,
        );
        assert_eq!(included.len(), 1);
        assert!(included[0].content.contains("Shared rules content"));
        assert!(included[0].parent.is_some());
    }

    #[test]
    fn at_include_ignores_unsupported_extensions() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();

        std::fs::write(root.join("script.rs"), "fn main() {}").unwrap();
        std::fs::write(root.join("CLAUDE.md"), "@./script.rs\nContent").unwrap();

        let mut processed = HashSet::new();
        let included = resolve_includes(
            &std::fs::read_to_string(root.join("CLAUDE.md")).unwrap(),
            root,
            MemoryType::Project,
            &mut processed,
        );
        // .rs is not a supported extension for includes
        assert!(included.is_empty());
    }

    #[test]
    fn load_memory_files_integration() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();

        // Create project CLAUDE.md
        std::fs::write(
            root.join("CLAUDE.md"),
            "---\nname: proj\n---\nProject rules",
        )
        .unwrap();

        let sources = cc_config::SettingsSourcesEnabled {
            user: false, // don't touch real ~/.claude/memory/
            project: true,
            local: true,
        };
        let files = load_memory_files(root, root, &sources);
        assert!(files.iter().any(|f| f.content.contains("Project rules")));
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

    // M6 regression: `@/abs/path` resolves and reads the file.
    #[test]
    fn at_include_absolute_path_is_expanded() {
        let dir = tempfile::TempDir::new().unwrap();
        let inc = dir.path().join("abs.md");
        std::fs::write(&inc, "ABSOLUTE-BODY").unwrap();

        let claudemd = dir.path().join("CLAUDE.md");
        std::fs::write(&claudemd, format!("@{}\ntail", inc.display())).unwrap();

        let mut processed = HashSet::new();
        let included = resolve_includes(
            &std::fs::read_to_string(&claudemd).unwrap(),
            dir.path(),
            MemoryType::Project,
            &mut processed,
        );
        assert_eq!(included.len(), 1);
        assert!(
            included[0].content.contains("ABSOLUTE-BODY"),
            "expected absolute-form @-include to be read; got: {:?}",
            included[0].content
        );
    }

    // M6 regression: the dedup key falls back to a lexical path when the
    // file doesn't exist (so `canonicalize()` / `metadata()` would both
    // fail). Prior code used `canonicalize().unwrap_or(path)` which could
    // yield distinct keys for the same logical path accessed via different
    // syntactic forms, silently bypassing cycle detection.
    #[test]
    fn include_dedup_key_lexical_fallback_on_missing_file() {
        let key = include_dedup_key(Path::new("/definitely/not/a/real/path.md"));
        assert!(matches!(key, IncludeKey::Lexical(_)));
    }

    #[test]
    fn lexical_normalize_collapses_dot_and_dotdot() {
        assert_eq!(
            lexical_normalize(Path::new("/a/b/./c")),
            PathBuf::from("/a/b/c")
        );
        assert_eq!(
            lexical_normalize(Path::new("/a/b/../c")),
            PathBuf::from("/a/c")
        );
    }
}
