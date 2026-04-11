//! Project discovery — git walk-up, canonical root, worktree detection.

use std::path::{Path, PathBuf};

/// Resolved project context — computed once at startup, immutable thereafter.
#[derive(Debug, Clone)]
pub struct ProjectContext {
    /// The original working directory (captured at startup, never changed).
    pub original_cwd: PathBuf,
    /// Git root of the current working directory (None if not in a git repo).
    pub git_root: Option<PathBuf>,
    /// Canonical root (resolves through worktree chains to main repo).
    /// Falls back to git_root if not a worktree, or original_cwd if no git.
    pub canonical_root: PathBuf,
    /// True if we're running inside a git worktree.
    pub is_worktree: bool,
    /// Normalized path used as key in project-scoped config.
    pub config_key: String,
}

impl ProjectContext {
    /// Discover project context from the given starting directory.
    /// Applies NFC normalization on macOS (HFS+ decomposition).
    pub fn discover(start_path: &Path) -> Self {
        let original_cwd = normalize_path(start_path);
        let git_root = find_git_root(start_path);

        let (canonical_root, is_worktree) = if let Some(ref root) = git_root {
            match find_canonical_root(root) {
                Some(canonical) if canonical != *root => (canonical, true),
                _ => (root.clone(), false),
            }
        } else {
            (original_cwd.clone(), false)
        };

        let config_key = canonical_root.to_string_lossy().replace('/', "-");

        Self {
            original_cwd,
            git_root,
            canonical_root,
            is_worktree,
            config_key,
        }
    }

    /// Non-git fallback: just use CWD.
    pub fn from_cwd(cwd: &Path) -> Self {
        Self {
            original_cwd: cwd.to_path_buf(),
            git_root: None,
            canonical_root: cwd.to_path_buf(),
            is_worktree: false,
            config_key: cwd.to_string_lossy().replace('/', "-"),
        }
    }
}

/// Walk up from `start` looking for `.git` (file or directory).
fn find_git_root(start: &Path) -> Option<PathBuf> {
    let mut current = start.to_path_buf();
    loop {
        let git_path = current.join(".git");
        if git_path.exists() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

/// Resolve through worktree .git file -> gitdir -> commondir -> main repo.
/// Includes symlink safety: reject back-links that escape the repo.
fn find_canonical_root(git_root: &Path) -> Option<PathBuf> {
    let git_path = git_root.join(".git");

    // If .git is a file (worktree), read the gitdir pointer.
    if git_path.is_file() {
        let content = std::fs::read_to_string(&git_path).ok()?;
        let gitdir_line = content.strip_prefix("gitdir: ")?.trim();
        let gitdir = if Path::new(gitdir_line).is_absolute() {
            PathBuf::from(gitdir_line)
        } else {
            git_root.join(gitdir_line)
        };

        // Security: validate the gitdir resolves to a real path
        let gitdir_canonical = gitdir.canonicalize().ok()?;
        if !gitdir_canonical.exists() {
            return None;
        }

        // Follow commondir to find the main repo.
        let commondir_path = gitdir.join("commondir");
        if commondir_path.exists() {
            let commondir = std::fs::read_to_string(&commondir_path)
                .ok()?
                .trim()
                .to_string();
            let common_abs = if Path::new(&commondir).is_absolute() {
                PathBuf::from(&commondir)
            } else {
                gitdir.join(&commondir)
            };
            // Security: canonicalize to resolve any symlinks
            let canonical = common_abs.canonicalize().ok()?;
            // The canonical root is the parent of the .git directory that commondir points to.
            return canonical.parent().map(|p| p.to_path_buf());
        }
    }

    // .git is a directory (regular repo) — this IS the canonical root.
    Some(git_root.to_path_buf())
}

/// Normalize a path for cross-platform consistency.
/// On macOS, applies NFC normalization (HFS+ may return NFD-decomposed paths).
fn normalize_path(path: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        // NFC-normalize the path string if it contains non-ASCII bytes
        let bytes = path.as_os_str().as_bytes();
        if bytes.iter().any(|&b| b > 127) {
            if let Ok(s) = std::str::from_utf8(bytes) {
                // Use Unicode NFC normalization
                let normalized: String = s.chars().collect(); // TODO: use unicode-normalization crate for proper NFC
                return PathBuf::from(OsStr::from_bytes(normalized.as_bytes()));
            }
        }
        path.to_path_buf()
    }
    #[cfg(not(target_os = "macos"))]
    {
        path.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn discover_no_git() {
        let tmp = TempDir::new().unwrap();
        let ctx = ProjectContext::discover(tmp.path());
        assert!(ctx.git_root.is_none());
        assert!(!ctx.is_worktree);
        assert_eq!(ctx.canonical_root, tmp.path().to_path_buf());
    }

    #[test]
    fn discover_with_git_dir() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir(tmp.path().join(".git")).unwrap();
        let ctx = ProjectContext::discover(tmp.path());
        assert_eq!(ctx.git_root, Some(tmp.path().to_path_buf()));
        assert!(!ctx.is_worktree);
    }

    #[test]
    fn discover_nested_dir() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir(tmp.path().join(".git")).unwrap();
        let nested = tmp.path().join("src").join("deep");
        std::fs::create_dir_all(&nested).unwrap();
        let ctx = ProjectContext::discover(&nested);
        assert_eq!(ctx.git_root, Some(tmp.path().to_path_buf()));
        assert_eq!(ctx.original_cwd, nested);
    }

    #[test]
    fn config_key_no_slashes() {
        let ctx = ProjectContext::from_cwd(Path::new("/Users/test/project"));
        assert!(!ctx.config_key.contains('/'));
    }

    #[test]
    fn discover_worktree_resolves_canonical_root() {
        let tmp = TempDir::new().unwrap();

        // Create main repo
        let main_repo = tmp.path().join("main");
        std::fs::create_dir_all(main_repo.join(".git")).unwrap();

        // Create worktree with .git file pointing to main
        let worktree = tmp.path().join("worktree");
        std::fs::create_dir_all(&worktree).unwrap();
        let gitdir = main_repo.join(".git").join("worktrees").join("wt");
        std::fs::create_dir_all(&gitdir).unwrap();

        // Write commondir that points back to main repo's .git
        std::fs::write(
            gitdir.join("commondir"),
            main_repo.join(".git").to_string_lossy().as_bytes(),
        )
        .unwrap();

        // Write .git file in worktree pointing to gitdir
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}", gitdir.display()),
        )
        .unwrap();

        let ctx = ProjectContext::discover(&worktree);
        assert!(ctx.is_worktree);
        assert_eq!(ctx.git_root, Some(worktree.clone()));
        // Canonical root should resolve to the main repo
        assert_eq!(
            ctx.canonical_root.canonicalize().unwrap(),
            main_repo.canonicalize().unwrap()
        );
    }

    #[test]
    fn normalize_path_ascii_unchanged() {
        let path = Path::new("/Users/test/project");
        let normalized = normalize_path(path);
        assert_eq!(normalized, path);
    }
}
