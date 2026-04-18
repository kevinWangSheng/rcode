use std::path::Path;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;
use tracing::{debug, warn};

/// Default timeout for every `git` subprocess invocation in this crate.
///
/// `check-ignore --stdin` (and the other `git` calls used during system-prompt
/// assembly) can hang indefinitely on corrupt indexes or very large repos.
/// Every call site runs under this bound and degrades gracefully on elapse.
const DEFAULT_GIT_TIMEOUT_MS: u64 = 5_000;

/// Resolve the git subprocess timeout from the environment.
///
/// Reads `CC_GIT_IGNORE_TIMEOUT_MS` at call time (so tests can override it
/// per invocation) and falls back to [`DEFAULT_GIT_TIMEOUT_MS`] on any parse
/// failure or missing value.
fn git_timeout() -> Duration {
    let ms = std::env::var("CC_GIT_IGNORE_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_GIT_TIMEOUT_MS);
    Duration::from_millis(ms)
}

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
    ///
    /// Every subprocess call runs under [`git_timeout`]; a slow or hung `git`
    /// yields an empty (default) context rather than blocking CLI startup.
    pub async fn collect(cwd: &Path) -> Self {
        let mut ctx = GitContext::default();
        let bound = git_timeout();

        // Check if we're in a git repo
        let is_git = run_git_output(
            &["rev-parse", "--is-inside-work-tree"],
            cwd,
            bound,
            "rev-parse --is-inside-work-tree",
        )
        .await
        .map(|o| o.status.success())
        .unwrap_or(false);

        if !is_git {
            debug!("not in a git repository");
            return ctx;
        }

        // Get repo root
        if let Some(output) = run_git_output(
            &["rev-parse", "--show-toplevel"],
            cwd,
            bound,
            "rev-parse --show-toplevel",
        )
        .await
        {
            if output.status.success() {
                ctx.repo_root = Some(String::from_utf8_lossy(&output.stdout).trim().to_string());
            }
        }

        // Get current branch
        if let Some(output) = run_git_output(
            &["rev-parse", "--abbrev-ref", "HEAD"],
            cwd,
            bound,
            "rev-parse --abbrev-ref HEAD",
        )
        .await
        {
            if output.status.success() {
                let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
                ctx.branch = Some(branch);
            }
        }

        // Get recent commits (last 5)
        if let Some(output) =
            run_git_output(&["log", "--oneline", "-5"], cwd, bound, "log --oneline -5").await
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

/// Run `git <args>` under a bounded timeout, returning the captured output.
///
/// Returns `None` on spawn failure, I/O error, or timeout. On timeout the
/// child is killed and a `warn!` is emitted naming the subcommand and the
/// timeout value. This is the shared helper behind every git shell-out in
/// this crate — all call sites degrade gracefully rather than hanging.
async fn run_git_output(
    args: &[&str],
    cwd: &Path,
    bound: Duration,
    label: &str,
) -> Option<std::process::Output> {
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(cwd);

    let fut = cmd.output();
    match timeout(bound, fut).await {
        Ok(Ok(o)) => Some(o),
        Ok(Err(err)) => {
            debug!(subcommand = label, error = %err, "git subprocess failed");
            None
        }
        Err(_) => {
            warn!(
                subcommand = label,
                timeout_ms = bound.as_millis() as u64,
                "git subprocess timed out; degrading to empty result",
            );
            None
        }
    }
}

/// Check whether the given directory is a bare git repository.
///
/// Returns `false` if git is unavailable, the directory is not a git repo,
/// or the subprocess exceeds [`git_timeout`].
pub async fn is_bare_repo(cwd: &Path) -> bool {
    match run_git_output(
        &["rev-parse", "--is-bare-repository"],
        cwd,
        git_timeout(),
        "rev-parse --is-bare-repository",
    )
    .await
    {
        Some(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim() == "true",
        _ => false,
    }
}

/// Check whether a path is ignored by git (via `.gitignore` rules).
///
/// Uses `git check-ignore -q -- <path>` and treats exit code 0 as ignored.
/// Returns `false` if git is unavailable, the path is not in a repo, the
/// subprocess times out, or any other error occurs — we never exclude files
/// by mistake.
pub async fn is_git_ignored(path: &Path, cwd: &Path) -> bool {
    let path_str = path.to_string_lossy();
    match run_git_output(
        &["check-ignore", "-q", "--", path_str.as_ref()],
        cwd,
        git_timeout(),
        "check-ignore -q",
    )
    .await
    {
        // Exit 0 = ignored, exit 1 = not ignored, other = error (treat as not ignored)
        Some(o) => o.status.code() == Some(0),
        None => false,
    }
}

/// Check whether multiple paths are git-ignored, returning per-path booleans.
///
/// More efficient than calling `is_git_ignored` in a loop for many files.
/// Runs a single `git check-ignore --stdin` invocation under [`git_timeout`].
///
/// On timeout the child is killed and every path is reported as not-ignored
/// (i.e. `vec![false; paths.len()]`) so a hung `git` degrades to "no filter
/// applied" rather than blocking CLI startup. A `warn!` log names the
/// timeout value and the path count.
pub async fn filter_git_ignored(paths: &[&Path], cwd: &Path) -> Vec<bool> {
    if paths.is_empty() {
        return Vec::new();
    }

    let bound = git_timeout();

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
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return vec![false; paths.len()],
    };

    // Write stdin (bounded by the same timeout so a wedged pipe can't hang)
    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        let write_fut = async move {
            let _ = stdin.write_all(stdin_input.as_bytes()).await;
            // stdin closed on drop
        };
        if timeout(bound, write_fut).await.is_err() {
            warn!(
                subcommand = "check-ignore --stdin",
                timeout_ms = bound.as_millis() as u64,
                paths = paths.len(),
                "git check-ignore stdin write timed out; treating all paths as not ignored",
            );
            let _ = child.kill().await;
            return vec![false; paths.len()];
        }
    }

    let result = match timeout(bound, child.wait_with_output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(_)) => return vec![false; paths.len()],
        Err(_) => {
            // Timed out waiting for git to finish; `kill_on_drop` will reap the
            // child when `child` is dropped at the end of the Err branch, but
            // we no longer hold it here (moved into `wait_with_output`). The
            // spawned git process receives SIGKILL via the tokio runtime
            // because `kill_on_drop(true)` was set.
            warn!(
                subcommand = "check-ignore --stdin",
                timeout_ms = bound.as_millis() as u64,
                paths = paths.len(),
                "git check-ignore timed out; treating all paths as not ignored",
            );
            return vec![false; paths.len()];
        }
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

    /// Single tokio async mutex shared by all tests that invoke `git` via
    /// PATH or mutate process-wide env (PATH / `CC_GIT_IGNORE_TIMEOUT_MS`).
    fn git_test_lock() -> &'static tokio::sync::Mutex<()> {
        use std::sync::OnceLock;
        use tokio::sync::Mutex;
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    /// Async variant — acquire inside `#[tokio::test]`s. Holding this guard
    /// across `.await` is intentional (it's a tokio `MutexGuard`, not a
    /// `std::sync::MutexGuard`) so serialization survives yields.
    async fn git_test_guard() -> tokio::sync::MutexGuard<'static, ()> {
        git_test_lock().lock().await
    }

    /// Sync variant — acquire inside `#[test]`s. Uses `blocking_lock` which
    /// panics if called from inside a tokio runtime; the sync tests below
    /// don't run under tokio, so this is safe.
    fn git_test_guard_blocking() -> tokio::sync::MutexGuard<'static, ()> {
        git_test_lock().blocking_lock()
    }

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
        let _g = git_test_guard().await;
        // /tmp is virtually never a git repo on macOS / Linux build hosts.
        let ctx = GitContext::collect(std::path::Path::new("/tmp")).await;
        let _ = ctx.to_system_text();
    }

    #[tokio::test]
    async fn collect_in_real_git_repo() {
        let _g = git_test_guard().await;
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
        let _g = git_test_guard().await;
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
        let _g = git_test_guard().await;
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(!is_bare_repo(tmp.path()).await);
    }

    #[tokio::test]
    async fn is_bare_repo_returns_false_for_regular_repo() {
        let _g = git_test_guard().await;
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
        let _g = git_test_guard().await;
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
        let _g = git_test_guard().await;
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
        let _g = git_test_guard().await;
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

    /// Simulate a hung `git` binary by prepending a PATH entry whose `git` is
    /// a shell script that sleeps longer than any reasonable test timeout.
    /// `filter_git_ignored` must kill the child and return `vec![false; N]`
    /// within well under the sleep duration.
    #[tokio::test]
    async fn filter_git_ignored_times_out_on_hung_git() {
        let _g = git_test_guard().await;
        // This test mutates process-global state (PATH + CC_GIT_IGNORE_TIMEOUT_MS).
        // Keep it self-contained and restore env on exit.
        let stub_dir = tempfile::TempDir::new().unwrap();
        let stub_git = stub_dir.path().join("git");

        // Fake git: sleep 30s regardless of args/stdin.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(&stub_git, "#!/bin/sh\nsleep 30\n").unwrap();
            let mut perm = std::fs::metadata(&stub_git).unwrap().permissions();
            perm.set_mode(0o755);
            std::fs::set_permissions(&stub_git, perm).unwrap();
        }
        #[cfg(not(unix))]
        {
            // PATH-based override is unreliable on Windows; skip.
            return;
        }

        let orig_path = std::env::var_os("PATH");
        let orig_timeout = std::env::var_os("CC_GIT_IGNORE_TIMEOUT_MS");

        // SAFETY: env access in tests — restored before returning.
        std::env::set_var(
            "PATH",
            std::env::join_paths(std::iter::once(stub_dir.path().to_path_buf()).chain(
                std::env::split_paths(orig_path.as_deref().unwrap_or_default()),
            ))
            .unwrap(),
        );
        std::env::set_var("CC_GIT_IGNORE_TIMEOUT_MS", "300");

        let cwd = tempfile::TempDir::new().unwrap();
        let p1 = cwd.path().join("a.rs");
        let p2 = cwd.path().join("b.rs");
        let paths: Vec<&Path> = vec![p1.as_path(), p2.as_path()];

        let started = std::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(6),
            filter_git_ignored(&paths, cwd.path()),
        )
        .await;
        let elapsed = started.elapsed();

        // Restore env first, then assert, so a failure doesn't poison other tests.
        match orig_path {
            Some(v) => std::env::set_var("PATH", v),
            None => std::env::remove_var("PATH"),
        }
        match orig_timeout {
            Some(v) => std::env::set_var("CC_GIT_IGNORE_TIMEOUT_MS", v),
            None => std::env::remove_var("CC_GIT_IGNORE_TIMEOUT_MS"),
        }

        let flags = result.expect("filter_git_ignored exceeded the outer 6s safety timeout");
        assert_eq!(flags, vec![false, false]);
        assert!(
            elapsed < Duration::from_secs(5),
            "filter_git_ignored took {elapsed:?}, expected to honor CC_GIT_IGNORE_TIMEOUT_MS=300ms",
        );
    }

    #[test]
    fn git_timeout_respects_env_override() {
        let _g = git_test_guard_blocking();
        let orig = std::env::var_os("CC_GIT_IGNORE_TIMEOUT_MS");

        std::env::set_var("CC_GIT_IGNORE_TIMEOUT_MS", "250");
        assert_eq!(git_timeout(), Duration::from_millis(250));

        std::env::remove_var("CC_GIT_IGNORE_TIMEOUT_MS");
        assert_eq!(git_timeout(), Duration::from_millis(DEFAULT_GIT_TIMEOUT_MS));

        // Malformed value falls back to the default.
        std::env::set_var("CC_GIT_IGNORE_TIMEOUT_MS", "not-a-number");
        assert_eq!(git_timeout(), Duration::from_millis(DEFAULT_GIT_TIMEOUT_MS));

        match orig {
            Some(v) => std::env::set_var("CC_GIT_IGNORE_TIMEOUT_MS", v),
            None => std::env::remove_var("CC_GIT_IGNORE_TIMEOUT_MS"),
        }
    }
}
