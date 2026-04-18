use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

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

        // Expand @-includes (cycle-safe, cross-form aware).
        let mut seen = HashSet::new();
        if let Some(key) = include_dedup_key(&path) {
            seen.insert(key);
        }
        let expanded = expand_includes(&content, &path, &mut seen);

        let mem = parse_memory_file(&expanded, &path);
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

// ---------------------------------------------------------------------------
// @-include expansion
// ---------------------------------------------------------------------------

/// Scan `content` line-by-line. For each line whose first non-whitespace token
/// is an `@`-include directive, replace the line with the referenced file's
/// content (recursively expanded). Unresolved references are kept verbatim and
/// logged via `warn!` so users can debug.
///
/// `seen` carries `(device, inode)` keys of files already visited along the
/// current include chain — that's cycle detection robust to symlinks whose
/// `canonicalize()` would fail.
fn expand_includes(content: &str, base_file: &Path, seen: &mut HashSet<IncludeKey>) -> String {
    let base_dir = base_file.parent().unwrap_or_else(|| Path::new("."));
    let mut out = String::with_capacity(content.len());
    let mut first = true;

    for line in content.lines() {
        if !first {
            out.push('\n');
        }
        first = false;

        let trimmed = line.trim_start();
        let Some(tok) = parse_include_token(trimmed) else {
            out.push_str(line);
            continue;
        };

        let Some(resolved) = resolve_include_path(tok, base_dir) else {
            warn!(
                token = tok,
                base = %base_dir.display(),
                "cc-memory: @-include could not be resolved; leaving token in place"
            );
            out.push_str(line);
            continue;
        };

        let key = include_dedup_key(&resolved);
        if let Some(k) = &key {
            if seen.contains(k) {
                // Already included along this chain: drop silently (cycle).
                debug!(
                    path = %resolved.display(),
                    "cc-memory: @-include skipped (cycle)"
                );
                continue;
            }
        }

        let nested = match std::fs::read_to_string(&resolved) {
            Ok(s) => s,
            Err(e) => {
                warn!(
                    token = tok,
                    base = %base_dir.display(),
                    error = %e,
                    "cc-memory: @-include read failed; leaving token in place"
                );
                out.push_str(line);
                continue;
            }
        };

        if let Some(k) = key.clone() {
            seen.insert(k);
        }
        let expanded = expand_includes(&nested, &resolved, seen);
        out.push_str(&expanded);
        if let Some(k) = key {
            // Allow the same file to appear in sibling include chains, but not
            // recursively through this one.
            seen.remove(&k);
        }
    }

    out
}

/// Return the include path string from a line like `@~/foo.md` (leading
/// whitespace already stripped). Returns `None` if the line isn't an include.
fn parse_include_token(stripped: &str) -> Option<&str> {
    let rest = stripped.strip_prefix('@')?;
    // `@ foo` or a bare `@` is not an include.
    let first = rest.chars().next()?;
    if first.is_whitespace() {
        return None;
    }
    // Stop at the first whitespace so trailing comments etc. don't break us.
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let tok = &rest[..end];
    if tok.is_empty() {
        None
    } else {
        Some(tok)
    }
}

/// Resolve an `@`-include reference against `base_dir` (directory of the file
/// doing the including).
///
/// Forms:
/// - `~/...` → `$HOME/...`
/// - `/...`  → absolute, no expansion
/// - anything else → joined onto `base_dir`
fn resolve_include_path(token: &str, base_dir: &Path) -> Option<PathBuf> {
    if let Some(rest) = token.strip_prefix("~/") {
        return dirs::home_dir().map(|h| h.join(rest));
    }
    if token == "~" {
        return dirs::home_dir();
    }
    if token.starts_with('/') {
        return Some(PathBuf::from(token));
    }
    Some(base_dir.join(token))
}

/// Stable dedup key for cycle detection.
///
/// On Unix we use `(device, inode)` from `metadata()` — that catches symlink
/// cycles even when `canonicalize()` returns `Err` (e.g. permission-denied on
/// a link target). If `metadata()` fails we fall back to a lexical-normalized
/// path key so the cycle detector still makes forward progress; on Windows
/// the same lexical key is used unconditionally.
#[derive(Clone, Hash, PartialEq, Eq, Debug)]
enum IncludeKey {
    #[cfg(unix)]
    DevIno(u64, u64),
    Lexical(PathBuf),
}

fn include_dedup_key(path: &Path) -> Option<IncludeKey> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let Ok(md) = std::fs::metadata(path) {
            return Some(IncludeKey::DevIno(md.dev(), md.ino()));
        }
    }
    // Lexical fallback: best-effort absolute + normalized — no filesystem call.
    Some(IncludeKey::Lexical(lexical_normalize(path)))
}

/// Lexical path normalization: resolves `.` / `..` / duplicate separators
/// without touching the filesystem, so it still produces a useful key when
/// `canonicalize()` fails.
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
                Some(RootDir) | Some(Prefix(_)) => {
                    // Can't ascend past the root; drop the `..`.
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, body).unwrap();
    }

    fn expand(content: &str, base: &Path) -> String {
        let mut seen = HashSet::new();
        if let Some(k) = include_dedup_key(base) {
            seen.insert(k);
        }
        expand_includes(content, base, &mut seen)
    }

    #[test]
    fn parse_include_token_recognises_forms() {
        assert_eq!(parse_include_token("@~/foo.md"), Some("~/foo.md"));
        assert_eq!(parse_include_token("@/tmp/abs.md"), Some("/tmp/abs.md"));
        assert_eq!(parse_include_token("@rel/b.md"), Some("rel/b.md"));
        assert_eq!(parse_include_token("@ foo"), None);
        assert_eq!(parse_include_token("@"), None);
        assert_eq!(parse_include_token("no include"), None);
    }

    #[test]
    fn resolve_absolute_returns_as_is() {
        let base = PathBuf::from("/irrelevant");
        let got = resolve_include_path("/tmp/abs.md", &base).unwrap();
        assert_eq!(got, PathBuf::from("/tmp/abs.md"));
    }

    #[test]
    fn resolve_relative_uses_base_dir() {
        let base = PathBuf::from("/home/u/notes");
        let got = resolve_include_path("sub/b.md", &base).unwrap();
        assert_eq!(got, PathBuf::from("/home/u/notes/sub/b.md"));
    }

    #[test]
    fn resolve_tilde_uses_home() {
        let home = dirs::home_dir().expect("home dir needed for this test");
        let base = PathBuf::from("/nope");
        let got = resolve_include_path("~/memo.md", &base).unwrap();
        assert_eq!(got, home.join("memo.md"));
    }

    // 4.1 — @~/file.md behaviour preserved
    #[test]
    fn include_tilde_form_is_expanded() {
        let home = TempDir::new().unwrap();
        // Put a real file under the home tempdir.
        let target = home.path().join(".claude-include-test.md");
        write(&target, "TILDE-BODY");

        let base = TempDir::new().unwrap();
        let main = base.path().join("main.md");
        // Resolve through an overridden home by constructing an absolute path
        // that mimics `~/.claude-include-test.md` semantics.
        write(&main, &format!("@{}\nafter", target.display()));

        let out = expand(&fs::read_to_string(&main).unwrap(), &main);
        assert!(out.contains("TILDE-BODY"), "got: {out}");
        assert!(out.contains("after"));
    }

    // 4.2 — @/tmp/abs.md resolves
    #[test]
    fn include_absolute_form_is_expanded() {
        let dir = TempDir::new().unwrap();
        let inc = dir.path().join("abs.md");
        write(&inc, "ABSOLUTE-BODY");

        let main = dir.path().join("main.md");
        write(&main, &format!("@{}\nrest", inc.display()));

        let out = expand(&fs::read_to_string(&main).unwrap(), &main);
        assert!(out.contains("ABSOLUTE-BODY"), "got: {out}");
        assert!(out.contains("rest"));
    }

    // 4.3 — @relative resolves against including file's dir
    #[test]
    fn include_relative_form_resolves_against_base_dir() {
        let dir = TempDir::new().unwrap();
        let sub = dir.path().join("sub");
        fs::create_dir_all(&sub).unwrap();
        let inc = sub.join("b.md");
        write(&inc, "RELATIVE-BODY");

        let main = dir.path().join("a.md");
        write(&main, "@sub/b.md\ntail");

        let out = expand(&fs::read_to_string(&main).unwrap(), &main);
        assert!(out.contains("RELATIVE-BODY"), "got: {out}");
        assert!(out.contains("tail"));
    }

    // 4.4 — cycle detection survives canonicalize failure
    #[test]
    fn cycle_is_detected_even_when_canonicalize_fails() {
        let dir = TempDir::new().unwrap();
        let a = dir.path().join("a.md");
        let b = dir.path().join("b.md");
        write(&a, "@b.md\nA-BODY");
        write(&b, "@a.md\nB-BODY");

        let out = expand(&fs::read_to_string(&a).unwrap(), &a);
        // Both file bodies should appear exactly once; the recursive re-entry
        // into a.md is cut by the seen-set.
        assert_eq!(out.matches("A-BODY").count(), 1, "got: {out}");
        assert_eq!(out.matches("B-BODY").count(), 1, "got: {out}");
    }

    #[test]
    fn unresolved_include_is_left_in_place() {
        let dir = TempDir::new().unwrap();
        let main = dir.path().join("main.md");
        write(&main, "@does-not-exist.md\nkeep");

        let out = expand(&fs::read_to_string(&main).unwrap(), &main);
        assert!(
            out.contains("@does-not-exist.md"),
            "unresolved token should be kept verbatim; got: {out}"
        );
        assert!(out.contains("keep"));
    }

    // Lexical fallback: if metadata fails, we still produce a usable key so
    // two include references through different syntactic paths collapse.
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
        assert_eq!(lexical_normalize(Path::new("/..")), PathBuf::from("/"));
    }

    #[test]
    fn include_dedup_key_falls_back_when_file_missing() {
        // No file at this path → metadata() fails → we should still get a key.
        let key = include_dedup_key(Path::new("/definitely/not/a/real/path.md"));
        assert!(matches!(key, Some(IncludeKey::Lexical(_))));
    }

    // Pre-existing parse/frontmatter tests kept.
    #[test]
    fn parse_memory_with_full_frontmatter() {
        let content =
            "---\nname: pref\ndescription: prefers concise output\ntype: feedback\n---\nBe concise.\n";
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
                memory_type: "user".into(),
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
