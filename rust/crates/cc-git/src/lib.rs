use std::path::Path;
use tokio::process::Command;
use tracing::debug;

/// Git context included in the system prompt.
#[derive(Debug, Clone, Default)]
pub struct GitContext {
    pub branch: Option<String>,
    pub recent_commits: Vec<String>,
    pub repo_root: Option<String>,
}

impl GitContext {
    /// Collect git context from the given directory.
    /// Returns a default (empty) context if not in a git repo or git is unavailable.
    pub async fn collect(cwd: &Path) -> Self {
        let mut ctx = GitContext::default();

        // Check if we're in a git repo
        let is_git = Command::new("git")
            .args(["rev-parse", "--is-inside-work-tree"])
            .current_dir(cwd)
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false);

        if !is_git {
            debug!("not in a git repository");
            return ctx;
        }

        // Get repo root
        if let Ok(output) = Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .current_dir(cwd)
            .output()
            .await
        {
            if output.status.success() {
                ctx.repo_root =
                    Some(String::from_utf8_lossy(&output.stdout).trim().to_string());
            }
        }

        // Get current branch
        if let Ok(output) = Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(cwd)
            .output()
            .await
        {
            if output.status.success() {
                let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
                ctx.branch = Some(branch);
            }
        }

        // Get recent commits (last 5)
        if let Ok(output) = Command::new("git")
            .args(["log", "--oneline", "-5"])
            .current_dir(cwd)
            .output()
            .await
        {
            if output.status.success() {
                ctx.recent_commits = String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .map(|l| l.to_string())
                    .collect();
            }
        }

        ctx
    }

    /// Format as a system prompt block.
    pub fn to_system_text(&self) -> Option<String> {
        if self.branch.is_none() && self.recent_commits.is_empty() {
            return None;
        }

        let mut parts = Vec::new();
        if let Some(branch) = &self.branch {
            parts.push(format!("Current git branch: {branch}"));
        }
        if let Some(root) = &self.repo_root {
            parts.push(format!("Git repo root: {root}"));
        }
        if !self.recent_commits.is_empty() {
            parts.push(format!(
                "Recent commits:\n{}",
                self.recent_commits
                    .iter()
                    .map(|c| format!("  {c}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        }
        Some(parts.join("\n"))
    }
}

/// Check whether the given directory is a bare git repository.
///
/// Returns `false` if git is unavailable or the directory is not a git repo.
pub async fn is_bare_repo(cwd: &Path) -> bool {
    let output = Command::new("git")
        .args(["rev-parse", "--is-bare-repository"])
        .current_dir(cwd)
        .output()
        .await;

    match output {
        Ok(o) if o.status.success() => {
            String::from_utf8_lossy(&o.stdout).trim() == "true"
        }
        _ => false,
    }
}

/// Check whether a path is ignored by git (via `.gitignore` rules).
///
/// Uses `git check-ignore -q -- <path>` and treats exit code 0 as ignored.
/// Returns `false` if git is unavailable, the path is not in a repo, or any
/// other error occurs — we never exclude files by mistake.
pub async fn is_git_ignored(path: &Path, cwd: &Path) -> bool {
    let path_str = path.to_string_lossy();
    let output = Command::new("git")
        .args(["check-ignore", "-q", "--", path_str.as_ref()])
        .current_dir(cwd)
        .output()
        .await;

    match output {
        // Exit 0 = ignored, exit 1 = not ignored, other = error (treat as not ignored)
        Ok(o) => o.status.code() == Some(0),
        Err(_) => false,
    }
}

/// Check whether multiple paths are git-ignored, returning per-path booleans.
///
/// More efficient than calling `is_git_ignored` in a loop for many files.
/// Runs a single `git check-ignore --stdin` invocation.
pub async fn filter_git_ignored(paths: &[&Path], cwd: &Path) -> Vec<bool> {
    if paths.is_empty() {
        return Vec::new();
    }

    // Build stdin: one path per line (newline-separated, no -z flag)
    let path_strings: Vec<String> = paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let stdin_input = path_strings.join("\n") + "\n";

    let mut child = match Command::new("git")
        .args(["check-ignore", "--stdin"])
        .current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return vec![false; paths.len()],
    };

    // Write stdin
    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        let _ = stdin.write_all(stdin_input.as_bytes()).await;
        // stdin closed on drop
    }

    let result = match child.wait_with_output().await {
        Ok(o) => o,
        Err(_) => return vec![false; paths.len()],
    };

    // Output is newline-separated list of ignored paths (only ignored paths are printed)
    let ignored_set: std::collections::HashSet<String> = String::from_utf8_lossy(&result.stdout)
        .lines()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();

    path_strings
        .iter()
        .map(|p| ignored_set.contains(p.as_str()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_context_produces_no_text() {
        let ctx = GitContext::default();
        assert!(ctx.to_system_text().is_none());
    }

    #[test]
    fn populated_context_includes_branch_and_commits() {
        let ctx = GitContext {
            branch: Some("main".into()),
            repo_root: Some("/tmp/repo".into()),
            recent_commits: vec!["abc123 Fix bug".into(), "def456 Add feature".into()],
        };
        let text = ctx.to_system_text().expect("some text");
        assert!(text.contains("Current git branch: main"));
        assert!(text.contains("/tmp/repo"));
        assert!(text.contains("abc123 Fix bug"));
        assert!(text.contains("def456 Add feature"));
    }

    #[tokio::test]
    async fn collect_outside_git_repo_is_safe() {
        // /tmp is virtually never a git repo on macOS / Linux build hosts.
        let ctx = GitContext::collect(std::path::Path::new("/tmp")).await;
        let _ = ctx.to_system_text();
    }

    #[tokio::test]
    async fn collect_in_real_git_repo() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = tmp.path();

        let init = std::process::Command::new("git")
            .args(["init"])
            .current_dir(repo)
            .output();

        let Ok(output) = init else { return };
        if !output.status.success() {
            return;
        }

        let _ = std::process::Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(repo)
            .output();
        let _ = std::process::Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(repo)
            .output();

        std::fs::write(repo.join("README.md"), "hello").unwrap();
        let _ = std::process::Command::new("git")
            .args(["add", "README.md"])
            .current_dir(repo)
            .output();
        let _ = std::process::Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(repo)
            .output();

        let ctx = GitContext::collect(repo).await;
        assert!(ctx.branch.is_some());
        assert!(ctx.repo_root.is_some());
        assert!(!ctx.recent_commits.is_empty());

        let text = ctx.to_system_text().expect("should have system text");
        assert!(text.contains("Current git branch:"));
        assert!(text.contains("Recent commits:"));
        assert!(text.contains("initial"));
    }

    #[tokio::test]
    async fn collect_in_nested_subdir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = tmp.path();

        let init = std::process::Command::new("git")
            .args(["init"])
            .current_dir(repo)
            .output();
        let Ok(output) = init else { return };
        if !output.status.success() {
            return;
        }

        let nested = repo.join("src").join("deep");
        std::fs::create_dir_all(&nested).unwrap();

        let ctx = GitContext::collect(&nested).await;
        assert!(ctx.repo_root.is_some());
    }

    #[tokio::test]
    async fn is_bare_repo_returns_false_for_non_repo() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(!is_bare_repo(tmp.path()).await);
    }

    #[tokio::test]
    async fn is_bare_repo_returns_false_for_regular_repo() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = tmp.path();

        let init = std::process::Command::new("git")
            .args(["init"])
            .current_dir(repo)
            .output();
        let Ok(output) = init else { return };
        if !output.status.success() {
            return;
        }

        assert!(!is_bare_repo(repo).await);
    }

    #[tokio::test]
    async fn is_bare_repo_returns_true_for_bare_repo() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bare = tmp.path().join("bare.git");
        std::fs::create_dir_all(&bare).unwrap();

        let init = std::process::Command::new("git")
            .args(["init", "--bare"])
            .current_dir(&bare)
            .output();
        let Ok(output) = init else { return };
        if !output.status.success() {
            return;
        }

        assert!(is_bare_repo(&bare).await);
    }

    #[tokio::test]
    async fn is_git_ignored_respects_gitignore() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = tmp.path();

        let init = std::process::Command::new("git")
            .args(["init"])
            .current_dir(repo)
            .output();
        let Ok(output) = init else { return };
        if !output.status.success() {
            return;
        }

        // Write .gitignore that ignores *.log files
        std::fs::write(repo.join(".gitignore"), "*.log\n").unwrap();
        std::fs::write(repo.join("app.log"), "log content").unwrap();
        std::fs::write(repo.join("app.rs"), "fn main() {}").unwrap();

        assert!(is_git_ignored(&repo.join("app.log"), repo).await);
        assert!(!is_git_ignored(&repo.join("app.rs"), repo).await);
    }

    #[tokio::test]
    async fn filter_git_ignored_returns_correct_flags() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = tmp.path();

        let init = std::process::Command::new("git")
            .args(["init"])
            .current_dir(repo)
            .output();
        let Ok(output) = init else { return };
        if !output.status.success() {
            return;
        }

        std::fs::write(repo.join(".gitignore"), "*.log\n").unwrap();
        std::fs::write(repo.join("debug.log"), "").unwrap();
        std::fs::write(repo.join("main.rs"), "").unwrap();

        let log_path = repo.join("debug.log");
        let rs_path = repo.join("main.rs");
        let paths: Vec<&Path> = vec![log_path.as_path(), rs_path.as_path()];
        let results = filter_git_ignored(&paths, repo).await;

        assert_eq!(results.len(), 2);
        assert!(results[0]); // debug.log is ignored
        assert!(!results[1]); // main.rs is not ignored
    }
}
