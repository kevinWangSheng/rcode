use std::path::PathBuf;
use tracing::debug;

/// A single memory file loaded from disk.
#[derive(Debug, Clone)]
pub struct MemoryFile {
    pub name: String,
    pub description: String,
    pub memory_type: String,
    pub body: String,
}

/// Load all memory files from `~/.claude/memory/*.md`.
/// Files without frontmatter are loaded as-is (graceful degradation per compatibility contract).
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

        let mem = parse_memory_file(&content, &path);
        memories.push(mem);
    }

    memories
}

fn memory_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("memory"))
}

/// Parse a memory file, extracting YAML frontmatter if present.
fn parse_memory_file(content: &str, path: &std::path::Path) -> MemoryFile {
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
                let memory_type =
                    extract_yaml_field(frontmatter, "type").unwrap_or_else(|| "user".into());

                return MemoryFile {
                    name,
                    description,
                    memory_type,
                    body,
                };
            }
        }
    }

    // No frontmatter — use filename as name, entire content as body
    MemoryFile {
        name: file_stem,
        description: String::new(),
        memory_type: "user".into(),
        body: content.to_string(),
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

/// Format loaded memories as a system prompt block.
pub fn memories_to_system_text(memories: &[MemoryFile]) -> Option<String> {
    if memories.is_empty() {
        return None;
    }

    let parts: Vec<String> = memories
        .iter()
        .map(|m| {
            if m.body.is_empty() {
                format!("## {}\n{}", m.name, m.description)
            } else {
                format!("## {}\n{}", m.name, m.body)
            }
        })
        .collect();

    Some(format!("# Memory\n\n{}", parts.join("\n\n")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn parse_memory_with_full_frontmatter() {
        let content = "---\nname: pref\ndescription: prefers concise output\ntype: feedback\n---\nBe concise.\n";
        let mem = parse_memory_file(content, &PathBuf::from("/tmp/pref.md"));
        assert_eq!(mem.name, "pref");
        assert_eq!(mem.memory_type, "feedback");
        assert_eq!(mem.description, "prefers concise output");
        assert_eq!(mem.body.trim(), "Be concise.");
    }

    #[test]
    fn parse_memory_without_frontmatter_uses_filename() {
        let mem = parse_memory_file("plain body", &PathBuf::from("/tmp/old.md"));
        assert_eq!(mem.name, "old");
        assert_eq!(mem.memory_type, "user");
        assert_eq!(mem.body, "plain body");
    }

    #[test]
    fn memories_to_system_text_aggregates_blocks() {
        let memories = vec![
            MemoryFile {
                name: "a".into(),
                description: "".into(),
                memory_type: "user".into(),
                body: "first".into(),
            },
            MemoryFile {
                name: "b".into(),
                description: "".into(),
                memory_type: "feedback".into(),
                body: "second".into(),
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
}
