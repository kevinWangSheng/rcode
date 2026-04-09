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
        // Either it's truly not a git repo (no branch) OR — on the off chance
        // /tmp happens to be inside one — `to_system_text` still returns valid
        // strings. Both outcomes are acceptable; the only thing we promise is
        // no panic and no crash.
        let _ = ctx.to_system_text();
    }
}
