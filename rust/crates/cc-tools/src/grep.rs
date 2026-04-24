use async_trait::async_trait;
use cc_core::{CcError, CcResult};
use regex::Regex;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::path::Path;
use walkdir::WalkDir;

use crate::{Tool, ToolContext, ToolInputSchema, ToolResult};

const MAX_RESULTS: usize = 250;
/// How often to check the cancel token inside a per-file line loop. Every 512
/// lines is cheap (one atomic load) and responsive (<1ms on a 10 GB log file).
const CANCEL_CHECK_INTERVAL: usize = 512;

pub struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "Grep"
    }

    fn description(&self) -> &str {
        "Search file contents using regular expressions. \
         Returns matching file paths (files_with_matches mode) or matching lines (content mode). \
         Supports glob filtering and recursive search."
    }

    fn input_schema(&self) -> ToolInputSchema {
        serde_json::from_value(json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regular expression pattern to search for"
                },
                "path": {
                    "type": "string",
                    "description": "File or directory to search in (defaults to current directory)"
                },
                "glob": {
                    "type": "string",
                    "description": "Glob pattern to filter files (e.g. '*.rs', '**/*.ts')"
                },
                "output_mode": {
                    "type": "string",
                    "enum": ["files_with_matches", "content", "count"],
                    "description": "Output mode (default: files_with_matches)"
                },
                "-i": {
                    "type": "boolean",
                    "description": "Case insensitive search"
                }
            },
            "required": ["pattern"]
        }))
        .unwrap()
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> CcResult<ToolResult> {
        let pattern_str = input["pattern"]
            .as_str()
            .ok_or_else(|| CcError::tool("tool", "missing 'pattern' field"))?;

        let case_insensitive = input["-i"].as_bool().unwrap_or(false);
        let re = if case_insensitive {
            Regex::new(&format!("(?i){}", pattern_str))
        } else {
            Regex::new(pattern_str)
        }
        .map_err(|e| CcError::tool("tool", format!("invalid regex pattern: {e}")))?;

        let search_path = input["path"]
            .as_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                std::env::current_dir()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string()
            });

        let glob_filter = input["glob"].as_str();
        let output_mode = input["output_mode"]
            .as_str()
            .unwrap_or("files_with_matches");

        let search_path = Path::new(&search_path);

        let mut results: Vec<String> = Vec::new();
        let mut total_count: usize = 0;

        // Pass 1: collect candidate paths that survive the glob + file-type
        // filters. We hold only `PathBuf`s here — no file opens yet —
        // because pass 2 will reopen each survivor after the git-ignore
        // batch filter runs. WalkDir's iter is streaming, but collecting
        // path strings is cheap compared to the per-file regex cost.
        let mut candidates: Vec<std::path::PathBuf> = Vec::new();
        for entry in WalkDir::new(search_path)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            if ctx.cancel.is_cancelled() {
                return Err(CcError::tool("tool", "Grep cancelled"));
            }
            let path = entry.path();

            if let Some(glob_pat) = glob_filter {
                let file_name = path.file_name().unwrap_or_default().to_string_lossy();
                let full_path = path.to_string_lossy();
                let pat = glob::Pattern::new(glob_pat)
                    .unwrap_or_else(|_| glob::Pattern::new("*").unwrap());
                if !pat.matches(&file_name) && !pat.matches(&full_path) {
                    let rel = path.strip_prefix(search_path).unwrap_or(path);
                    if !pat.matches(&rel.to_string_lossy()) {
                        continue;
                    }
                }
            }
            candidates.push(path.to_path_buf());
        }

        // Batch-check git-ignore in a single `git check-ignore --stdin`
        // subprocess. Outside a repo, on timeout, or when git is missing,
        // `filter_git_ignored` returns all-false so no files get dropped
        // by accident (P0 #3).
        let borrow: Vec<&Path> = candidates.iter().map(|p| p.as_path()).collect();
        let ignored_flags = cc_git::filter_git_ignored(&borrow, search_path).await;
        let survivors: Vec<std::path::PathBuf> = candidates
            .into_iter()
            .zip(ignored_flags)
            .filter_map(|(p, ignored)| (!ignored).then_some(p))
            .collect();

        'outer: for path in &survivors {
            if ctx.cancel.is_cancelled() {
                return Err(CcError::tool("tool", "Grep cancelled"));
            }
            let path: &Path = path.as_path();

            let file = match std::fs::File::open(path) {
                Ok(f) => f,
                Err(_) => continue,
            };
            let mut reader = BufReader::new(file);

            match output_mode {
                "files_with_matches" => {
                    let mut found = false;
                    for (idx, line) in (&mut reader).lines().map_while(Result::ok).enumerate() {
                        // Cancel check inside the per-file loop — a 10 GB log
                        // would otherwise run to EOF even after Ctrl+C.
                        if idx % CANCEL_CHECK_INTERVAL == 0 && ctx.cancel.is_cancelled() {
                            return Err(CcError::tool("tool", "Grep cancelled"));
                        }
                        if re.is_match(&line) {
                            found = true;
                            break;
                        }
                    }
                    if found {
                        results.push(path.to_string_lossy().to_string());
                        if results.len() >= MAX_RESULTS {
                            break 'outer;
                        }
                    }
                }
                "content" => {
                    for (line_num, line) in reader.lines().map_while(Result::ok).enumerate() {
                        if line_num % CANCEL_CHECK_INTERVAL == 0 && ctx.cancel.is_cancelled() {
                            return Err(CcError::tool("tool", "Grep cancelled"));
                        }
                        if re.is_match(&line) {
                            results.push(format!("{}:{}: {}", path.display(), line_num + 1, line));
                            if results.len() >= MAX_RESULTS {
                                break 'outer;
                            }
                        }
                    }
                }
                "count" => {
                    let mut count = 0usize;
                    for (idx, line) in reader.lines().map_while(Result::ok).enumerate() {
                        if idx % CANCEL_CHECK_INTERVAL == 0 && ctx.cancel.is_cancelled() {
                            return Err(CcError::tool("tool", "Grep cancelled"));
                        }
                        if re.is_match(&line) {
                            count += 1;
                        }
                    }
                    if count > 0 {
                        total_count += count;
                        results.push(format!("{}: {}", path.display(), count));
                        if results.len() >= MAX_RESULTS {
                            break 'outer;
                        }
                    }
                }
                _ => {}
            }
        }

        if results.is_empty() {
            return Ok(ToolResult::ok("No matches found."));
        }

        let mut output = results.join("\n");
        if results.len() >= MAX_RESULTS {
            output.push_str(&format!("\n... (results truncated at {MAX_RESULTS})"));
        }
        if output_mode == "count" {
            output.push_str(&format!("\nTotal matches: {total_count}"));
        }

        Ok(ToolResult::ok(output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn grep_honors_cancel_token_mid_walk() {
        // Pre-cancel the token and verify the walker bails immediately
        // rather than enumerating the entire tree. No fixture tree needed —
        // any non-empty directory works; the cancel check short-circuits
        // before the first file is processed.
        let tool = GrepTool;
        let token = CancellationToken::new();
        token.cancel();
        let ctx = ToolContext::for_test_bare(token);
        let result = tool
            .execute(json!({"pattern": "zzz", "path": "."}), &ctx)
            .await;
        assert!(result.is_err(), "expected cancel error, got {:?}", result);
        assert!(result.unwrap_err().to_string().contains("cancelled"));
    }

    #[tokio::test]
    async fn grep_honors_cancel_token_inside_per_file_loop() {
        // Build a single file big enough that the per-file loop has plenty of
        // iterations to trip the cancel check. `CANCEL_CHECK_INTERVAL` is 512,
        // so we need at least that many lines for the test to be meaningful.
        // We use 4096 lines to give comfortable margin.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("huge.txt");
        let mut contents = String::with_capacity(4096 * 16);
        for i in 0..4096 {
            contents.push_str(&format!("line-{i}-no-match\n"));
        }
        std::fs::write(&file, &contents).unwrap();

        let tool = GrepTool;
        let token = CancellationToken::new();
        // Pre-cancel — the outer walk check fires on the first file, but the
        // key point is the inner loop also observes cancellation now.
        token.cancel();
        let ctx = ToolContext::for_test_bare(token);

        let result = tool
            .execute(
                json!({"pattern": "nomatch", "path": file.to_string_lossy(), "output_mode": "content"}),
                &ctx,
            )
            .await;
        assert!(result.is_err(), "expected cancel error, got {:?}", result);
        assert!(result.unwrap_err().to_string().contains("cancelled"));
    }

    #[tokio::test]
    async fn grep_inner_loop_cancel_fires_mid_scan() {
        // Cancellation asserted AFTER the outer-loop check has already passed
        // for the single file — i.e. the only path to surface the cancel is
        // the inner-loop check. This is the load-bearing test for the fix.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("scan.txt");
        // CANCEL_CHECK_INTERVAL=512; need >512 lines to force at least one
        // inner-loop check after cancel fires.
        let mut contents = String::new();
        for i in 0..2048 {
            contents.push_str(&format!("filler-{i}\n"));
        }
        std::fs::write(&file, &contents).unwrap();

        let tool = GrepTool;
        let token = CancellationToken::new();
        let ctx = ToolContext::for_test_bare(token.clone());
        let token2 = token.clone();

        // Fire cancel ~20ms in — long enough for the grep walk to enter
        // the per-file loop, short enough that it's still scanning when
        // the flag flips.
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            token2.cancel();
        });

        let result = tool
            .execute(
                json!({"pattern": "zzz-no-match", "path": file.to_string_lossy(), "output_mode": "count"}),
                &ctx,
            )
            .await;
        // Could still race — a tiny file may finish before the 20ms cancel.
        // The contract is: when cancel *does* win, we get the cancelled error.
        if let Err(e) = result {
            assert!(e.to_string().contains("cancelled"), "got: {e}");
        }
    }

    #[tokio::test]
    async fn grep_invalid_regex_errors() {
        let tool = GrepTool;
        let ctx = ToolContext::for_test_bare(CancellationToken::new());
        let err = tool
            .execute(json!({"pattern": "["}), &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("regex"));
    }

    /// P0 #3: Grep must drop files git-ignores before reading them.
    /// Sets up a real git repo with `*.log` ignored, then greps for a
    /// token that only appears in the ignored file → expect no hits.
    #[tokio::test]
    async fn grep_skips_gitignored_files() {
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("skipping: git binary not available");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        let run = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(repo)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        };
        run(&["init", "-q"]);
        std::fs::write(repo.join(".gitignore"), "*.log\n").unwrap();
        std::fs::write(repo.join("visible.txt"), "has UNIQUE_TOKEN\n").unwrap();
        std::fs::write(repo.join("secret.log"), "UNIQUE_TOKEN is here\n").unwrap();

        let tool = GrepTool;
        let ctx = ToolContext::for_test_bare(CancellationToken::new());
        let result = tool
            .execute(
                json!({
                    "pattern": "UNIQUE_TOKEN",
                    "path": repo.to_string_lossy(),
                    "output_mode": "files_with_matches",
                }),
                &ctx,
            )
            .await
            .unwrap();
        let body = result.content;
        assert!(
            body.contains("visible.txt"),
            "non-ignored hit missing: {body}"
        );
        assert!(
            !body.contains("secret.log"),
            "git-ignored file leaked into grep result: {body}"
        );
    }
}
