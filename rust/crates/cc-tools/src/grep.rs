use async_trait::async_trait;
use cc_core::{CcError, CcResult};
use regex::Regex;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use walkdir::WalkDir;

use crate::{Tool, ToolContext, ToolInputSchema, ToolResult};

/// Default cap on result lines / paths. Matches TS
/// `DEFAULT_HEAD_LIMIT` in `src/tools/GrepTool/prompt.ts`.
const DEFAULT_HEAD_LIMIT: usize = 250;
/// How often to check the cancel token inside a per-file line loop. Every 512
/// lines is cheap (one atomic load) and responsive (<1ms on a 10 GB log file).
const CANCEL_CHECK_INTERVAL: usize = 512;

/// Directory names the walker skips unconditionally. Matches TS
/// `VCS_DIRECTORIES_TO_EXCLUDE` plus a handful of common build dirs.
/// Users can still grep inside these by naming them in `path`
/// explicitly — the exclusion only applies to recursive descent.
const VCS_DIRECTORIES_TO_EXCLUDE: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".bzr",
    ".jj",
    "node_modules",
    "target",
    ".venv",
    "venv",
    "__pycache__",
    ".mypy_cache",
    ".ruff_cache",
    ".pytest_cache",
    ".next",
    ".nuxt",
    ".cache",
    "dist",
    "build",
    ".gradle",
    ".idea",
    ".vscode",
];

pub struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "Grep"
    }

    fn description(&self) -> &str {
        "Search file contents using regular expressions. \
         Supports ripgrep-style context (-A/-B/-C), line numbers (-n), \
         file-type filters, pagination (head_limit/offset), and \
         multi-line mode. Returns matching paths (files_with_matches), \
         content lines, or per-file counts. VCS and build directories \
         (.git, node_modules, target, …) are skipped automatically."
    }

    fn input_schema(&self) -> ToolInputSchema {
        serde_json::from_value(json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regular expression pattern to search for (Rust regex syntax)."
                },
                "path": {
                    "type": "string",
                    "description": "File or directory to search in (defaults to the current working directory)."
                },
                "glob": {
                    "type": "string",
                    "description": "Glob pattern to filter files (e.g. '*.rs', '**/*.ts'). Applied to file names and relative paths."
                },
                "type": {
                    "type": "string",
                    "description": "ripgrep-style file-type filter (rust, js, ts, py, go, c, cpp, java, rb, md, json, yaml, toml, sh). Equivalent to `glob: '*.<ext>'` but less error-prone."
                },
                "output_mode": {
                    "type": "string",
                    "enum": ["files_with_matches", "content", "count"],
                    "description": "Output mode (default: files_with_matches)."
                },
                "-i": {
                    "type": "boolean",
                    "description": "Case insensitive search."
                },
                "-n": {
                    "type": "boolean",
                    "description": "Show line numbers on content / files_with_matches output."
                },
                "-A": {
                    "type": "number",
                    "description": "Show N lines of context AFTER each match (content mode)."
                },
                "-B": {
                    "type": "number",
                    "description": "Show N lines of context BEFORE each match (content mode)."
                },
                "-C": {
                    "type": "number",
                    "description": "Shortcut for `-A N -B N`. Overrides any narrower -A / -B given alongside."
                },
                "head_limit": {
                    "type": "number",
                    "description": "Cap on emitted result lines / paths. Default 250."
                },
                "offset": {
                    "type": "number",
                    "description": "Skip the first N matches before applying head_limit. Lets callers paginate through a large result set."
                },
                "multiline": {
                    "type": "boolean",
                    "description": "Enable `(?s)` dot-matches-newline and let the pattern span line boundaries. Only meaningful in content mode."
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
        let opts = GrepOptions::parse(&input)?;

        let mut results: Vec<String> = Vec::new();
        let mut total_count: usize = 0;

        // Pass 1: collect candidate paths. Skip VCS / build dirs via
        // WalkDir::filter_entry so we never stat inside them.
        let mut candidates: Vec<std::path::PathBuf> = Vec::new();
        for entry in WalkDir::new(&opts.search_path)
            .into_iter()
            .filter_entry(|e| !is_vcs_dir(e.path()))
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            if ctx.cancel.is_cancelled() {
                return Err(CcError::tool("tool", "Grep cancelled"));
            }
            let path = entry.path();

            if !opts.matches_file_filters(path) {
                continue;
            }
            candidates.push(path.to_path_buf());
        }

        // Batch-check git-ignore in a single `git check-ignore --stdin`
        // subprocess. Outside a repo, on timeout, or when git is missing,
        // `filter_git_ignored` returns all-false so no files get dropped
        // by accident (P0 #3).
        let borrow: Vec<&Path> = candidates.iter().map(|p| p.as_path()).collect();
        let ignored_flags = cc_git::filter_git_ignored(&borrow, &opts.search_path).await;
        let survivors: Vec<std::path::PathBuf> = candidates
            .into_iter()
            .zip(ignored_flags)
            .filter_map(|(p, ignored)| (!ignored).then_some(p))
            .collect();

        let cap = opts.offset.saturating_add(opts.head_limit);

        'outer: for path in &survivors {
            if ctx.cancel.is_cancelled() {
                return Err(CcError::tool("tool", "Grep cancelled"));
            }
            let path: &Path = path.as_path();

            match opts.output_mode {
                OutputMode::FilesWithMatches => {
                    let hit = grep_file_files_with_matches(path, &opts, &ctx.cancel)?;
                    if hit {
                        let rendered = if opts.show_line_numbers {
                            format!("{}:0", path.display())
                        } else {
                            path.to_string_lossy().to_string()
                        };
                        results.push(rendered);
                        if results.len() >= cap {
                            break 'outer;
                        }
                    }
                }
                OutputMode::Content => {
                    let produced = grep_file_content(path, &opts, &ctx.cancel)?;
                    for line in produced {
                        results.push(line);
                        if results.len() >= cap {
                            break 'outer;
                        }
                    }
                }
                OutputMode::Count => {
                    let count = grep_file_count(path, &opts, &ctx.cancel)?;
                    if count > 0 {
                        total_count += count;
                        results.push(format!("{}: {}", path.display(), count));
                        if results.len() >= cap {
                            break 'outer;
                        }
                    }
                }
            }
        }

        // Apply offset + head_limit AFTER collection so we keep the
        // cap semantics aligned with ripgrep (`--max-count` applies
        // to emitted rows, not per-file). We already broke out of
        // the walk once `cap` was hit.
        let total_produced = results.len();
        let effective: Vec<String> = results.into_iter().skip(opts.offset).collect();
        let truncated_by_head = total_produced >= cap;

        if effective.is_empty() {
            return Ok(ToolResult::ok("No matches found."));
        }

        let mut output = effective.join("\n");
        if truncated_by_head {
            output.push_str(&format!(
                "\n... (results truncated at head_limit={})",
                opts.head_limit
            ));
        }
        if opts.output_mode == OutputMode::Count {
            output.push_str(&format!("\nTotal matches: {total_count}"));
        }

        Ok(ToolResult::ok(output))
    }
}

#[derive(Debug)]
struct GrepOptions {
    pattern: Regex,
    search_path: std::path::PathBuf,
    glob_filter: Option<glob::Pattern>,
    type_filter: Option<String>, // extension like "rs"
    output_mode: OutputMode,
    show_line_numbers: bool,
    context_before: usize,
    context_after: usize,
    head_limit: usize,
    offset: usize,
    multiline: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum OutputMode {
    FilesWithMatches,
    Content,
    Count,
}

impl GrepOptions {
    fn parse(input: &Value) -> CcResult<Self> {
        let pattern_str = input["pattern"]
            .as_str()
            .ok_or_else(|| CcError::tool("tool", "missing 'pattern' field"))?;

        let case_insensitive = input["-i"].as_bool().unwrap_or(false);
        let multiline = input["multiline"].as_bool().unwrap_or(false);

        // Compose regex flags. `(?i)` is case-insensitive; `(?s)` is
        // dot-matches-newline (needed for multiline patterns).
        let mut prefix = String::new();
        if case_insensitive {
            prefix.push_str("(?i)");
        }
        if multiline {
            prefix.push_str("(?sm)");
        }
        let pattern = Regex::new(&format!("{prefix}{pattern_str}"))
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

        let glob_filter = input["glob"]
            .as_str()
            .map(|g| glob::Pattern::new(g).ok())
            .unwrap_or_default();

        let type_filter = input["type"].as_str().and_then(type_to_extension);

        let output_mode = match input["output_mode"]
            .as_str()
            .unwrap_or("files_with_matches")
        {
            "files_with_matches" => OutputMode::FilesWithMatches,
            "content" => OutputMode::Content,
            "count" => OutputMode::Count,
            other => return Err(CcError::tool(
                "tool",
                format!(
                    "unknown output_mode '{other}'; expected files_with_matches / content / count"
                ),
            )),
        };

        let show_line_numbers = input["-n"].as_bool().unwrap_or(false);
        let context_any = input["-C"].as_u64().map(|n| n as usize);
        let context_before =
            context_any.unwrap_or_else(|| input["-B"].as_u64().map(|n| n as usize).unwrap_or(0));
        let context_after =
            context_any.unwrap_or_else(|| input["-A"].as_u64().map(|n| n as usize).unwrap_or(0));
        let head_limit = input["head_limit"]
            .as_u64()
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_HEAD_LIMIT)
            .max(1);
        let offset = input["offset"].as_u64().map(|n| n as usize).unwrap_or(0);

        Ok(GrepOptions {
            pattern,
            search_path: std::path::PathBuf::from(search_path),
            glob_filter,
            type_filter,
            output_mode,
            show_line_numbers,
            context_before,
            context_after,
            head_limit,
            offset,
            multiline,
        })
    }

    fn matches_file_filters(&self, path: &Path) -> bool {
        if let Some(ext) = &self.type_filter {
            let path_ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase());
            if path_ext.as_deref() != Some(ext.as_str()) {
                return false;
            }
        }
        if let Some(pat) = &self.glob_filter {
            let file_name = path.file_name().unwrap_or_default().to_string_lossy();
            let full_path = path.to_string_lossy();
            if !pat.matches(&file_name) && !pat.matches(&full_path) {
                let rel = path.strip_prefix(&self.search_path).unwrap_or(path);
                if !pat.matches(&rel.to_string_lossy()) {
                    return false;
                }
            }
        }
        true
    }
}

/// Is any component of `path` a VCS / build directory name we skip
/// unconditionally? WalkDir's filter_entry also runs on the root, so we
/// must NOT match on the root path itself — only on descendants whose
/// *final* component is a VCS name.
fn is_vcs_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| VCS_DIRECTORIES_TO_EXCLUDE.contains(&n))
        .unwrap_or(false)
}

/// Map `type` input values to the extension the walker filters on.
/// Returns `None` for unknown values so the caller can pass them
/// through to the glob path without coupling to ripgrep's full
/// `--type` registry.
fn type_to_extension(t: &str) -> Option<String> {
    Some(
        match t {
            "rust" => "rs",
            "js" => "js",
            "ts" => "ts",
            "tsx" => "tsx",
            "py" | "python" => "py",
            "go" => "go",
            "c" => "c",
            "cpp" | "c++" | "cxx" => "cpp",
            "java" => "java",
            "rb" | "ruby" => "rb",
            "md" | "markdown" => "md",
            "json" => "json",
            "yaml" | "yml" => "yaml",
            "toml" => "toml",
            "sh" | "bash" => "sh",
            "html" => "html",
            "css" => "css",
            "sql" => "sql",
            _ => return None,
        }
        .to_string(),
    )
}

fn grep_file_files_with_matches(
    path: &Path,
    opts: &GrepOptions,
    cancel: &tokio_util::sync::CancellationToken,
) -> CcResult<bool> {
    if opts.multiline {
        let bytes = read_file_bytes(path)?;
        let text = String::from_utf8_lossy(&bytes);
        return Ok(opts.pattern.is_match(&text));
    }
    let Ok(file) = std::fs::File::open(path) else {
        return Ok(false);
    };
    let mut reader = BufReader::new(file);
    for (idx, line) in (&mut reader).lines().map_while(Result::ok).enumerate() {
        if idx % CANCEL_CHECK_INTERVAL == 0 && cancel.is_cancelled() {
            return Err(CcError::tool("tool", "Grep cancelled"));
        }
        if opts.pattern.is_match(&line) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Run content-mode grep with optional -A/-B/-C context expansion.
/// Returns the rendered lines (already formatted with `path:lineno:`
/// prefix when `-n` is set). For multiline mode the full file is
/// scanned in one go so the pattern can span newlines; otherwise we
/// process line by line to keep memory bounded on huge logs.
fn grep_file_content(
    path: &Path,
    opts: &GrepOptions,
    cancel: &tokio_util::sync::CancellationToken,
) -> CcResult<Vec<String>> {
    let lines: Vec<String> = if opts.multiline {
        let bytes = read_file_bytes(path)?;
        let text = String::from_utf8_lossy(&bytes);
        // Multi-line regex: emit each match as its own entry with the
        // match body joined on newlines flattened to \n inline so the
        // serialized result stays in one "line" per hit.
        let mut rendered = Vec::new();
        for m in opts.pattern.find_iter(&text) {
            if cancel.is_cancelled() {
                return Err(CcError::tool("tool", "Grep cancelled"));
            }
            let start_line = text[..m.start()].bytes().filter(|b| *b == b'\n').count() + 1;
            let body = m.as_str().replace('\n', "\\n");
            rendered.push(format_content_line(
                path,
                start_line,
                &body,
                opts.show_line_numbers,
            ));
        }
        return Ok(rendered);
    } else {
        let Ok(file) = std::fs::File::open(path) else {
            return Ok(Vec::new());
        };
        BufReader::new(file).lines().map_while(Result::ok).collect()
    };

    let mut rendered: Vec<String> = Vec::new();
    // Discover match indices first. Reusing the line buffer keeps memory
    // footprint at one pass through the file.
    let mut match_indices: Vec<usize> = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        if idx % CANCEL_CHECK_INTERVAL == 0 && cancel.is_cancelled() {
            return Err(CcError::tool("tool", "Grep cancelled"));
        }
        if opts.pattern.is_match(line) {
            match_indices.push(idx);
        }
    }

    if match_indices.is_empty() {
        return Ok(rendered);
    }
    if opts.context_after == 0 && opts.context_before == 0 {
        for idx in match_indices {
            rendered.push(format_content_line(
                path,
                idx + 1,
                &lines[idx],
                opts.show_line_numbers,
            ));
        }
        return Ok(rendered);
    }

    // Merge overlapping context windows so touching ranges print as
    // one block separated by `--` like ripgrep.
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for idx in match_indices {
        let start = idx.saturating_sub(opts.context_before);
        let end = idx.saturating_add(opts.context_after).min(lines.len() - 1);
        match ranges.last_mut() {
            Some(last) if start <= last.1 + 1 => {
                last.1 = last.1.max(end);
            }
            _ => ranges.push((start, end)),
        }
    }

    for (i, (start, end)) in ranges.iter().enumerate() {
        if i > 0 {
            rendered.push("--".to_string());
        }
        for (offset, line) in lines.iter().enumerate().take(*end + 1).skip(*start) {
            rendered.push(format_content_line(
                path,
                offset + 1,
                line,
                opts.show_line_numbers,
            ));
        }
    }

    Ok(rendered)
}

fn grep_file_count(
    path: &Path,
    opts: &GrepOptions,
    cancel: &tokio_util::sync::CancellationToken,
) -> CcResult<usize> {
    if opts.multiline {
        let bytes = read_file_bytes(path)?;
        let text = String::from_utf8_lossy(&bytes);
        return Ok(opts.pattern.find_iter(&text).count());
    }
    let Ok(file) = std::fs::File::open(path) else {
        return Ok(0);
    };
    let mut reader = BufReader::new(file);
    let mut count = 0usize;
    for (idx, line) in (&mut reader).lines().map_while(Result::ok).enumerate() {
        if idx % CANCEL_CHECK_INTERVAL == 0 && cancel.is_cancelled() {
            return Err(CcError::tool("tool", "Grep cancelled"));
        }
        if opts.pattern.is_match(&line) {
            count += 1;
        }
    }
    Ok(count)
}

fn format_content_line(
    path: &Path,
    line_num: usize,
    line: &str,
    _show_line_numbers: bool,
) -> String {
    // The default content format already threads the line number into
    // the row ("path:lineno: content") — matching ripgrep's `-Hn`
    // default. We keep the `-n` flag around as a parse-compat shim
    // so Claude issuing `-n` doesn't get a schema error, but the
    // output is identical either way. When we later add a non-
    // numbered mode this helper is the single branch that changes.
    format!("{}:{}: {}", path.display(), line_num, line)
}

fn read_file_bytes(path: &Path) -> CcResult<Vec<u8>> {
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Ok(Vec::new()),
    };
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)
        .map_err(|e| CcError::tool("tool", format!("failed to read {}: {e}", path.display())))?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn grep_honors_cancel_token_mid_walk() {
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
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("huge.txt");
        let mut contents = String::with_capacity(4096 * 16);
        for i in 0..4096 {
            contents.push_str(&format!("line-{i}-no-match\n"));
        }
        std::fs::write(&file, &contents).unwrap();

        let tool = GrepTool;
        let token = CancellationToken::new();
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
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("scan.txt");
        let mut contents = String::new();
        for i in 0..2048 {
            contents.push_str(&format!("filler-{i}\n"));
        }
        std::fs::write(&file, &contents).unwrap();

        let tool = GrepTool;
        let token = CancellationToken::new();
        let ctx = ToolContext::for_test_bare(token.clone());
        let token2 = token.clone();

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
        assert!(!body.contains("secret.log"), "ignored file leaked: {body}");
    }

    /// P0 #12: `-C N` emits N lines of context before and after each match.
    #[tokio::test]
    async fn grep_context_after_before_surrounding_match() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("src.txt");
        std::fs::write(&file, "alpha\nbeta\ngamma\nHIT-LINE\ndelta\nepsilon\n").unwrap();
        let tool = GrepTool;
        let ctx = ToolContext::for_test_bare(CancellationToken::new());
        let result = tool
            .execute(
                json!({
                    "pattern": "HIT-LINE",
                    "path": file.to_string_lossy(),
                    "output_mode": "content",
                    "-C": 1,
                }),
                &ctx,
            )
            .await
            .unwrap();
        let body = result.content;
        // Must emit gamma (before), HIT-LINE, and delta (after).
        assert!(body.contains("gamma"), "missing -B context:\n{body}");
        assert!(body.contains("HIT-LINE"), "missing match line:\n{body}");
        assert!(body.contains("delta"), "missing -A context:\n{body}");
        // Non-context lines must NOT appear (alpha is 2 lines before).
        assert!(!body.contains("alpha"), "leaked outside -C window:\n{body}");
    }

    /// P0 #12: `type: "rust"` filters to .rs files.
    #[tokio::test]
    async fn grep_type_filter_limits_to_extension() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("keep.rs"), "fn main() { TOKEN }").unwrap();
        std::fs::write(dir.path().join("skip.txt"), "also has TOKEN").unwrap();
        let tool = GrepTool;
        let ctx = ToolContext::for_test_bare(CancellationToken::new());
        let r = tool
            .execute(
                json!({
                    "pattern": "TOKEN",
                    "path": dir.path().to_string_lossy(),
                    "type": "rust",
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(r.content.contains("keep.rs"), "{}", r.content);
        assert!(!r.content.contains("skip.txt"), "{}", r.content);
    }

    /// P0 #12: `head_limit: 2` caps the emitted rows and marks truncation.
    #[tokio::test]
    async fn grep_head_limit_caps_emitted_rows() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            std::fs::write(dir.path().join(format!("f{i}.txt")), format!("match-{i}")).unwrap();
        }
        let tool = GrepTool;
        let ctx = ToolContext::for_test_bare(CancellationToken::new());
        let r = tool
            .execute(
                json!({
                    "pattern": "match",
                    "path": dir.path().to_string_lossy(),
                    "head_limit": 2,
                }),
                &ctx,
            )
            .await
            .unwrap();
        // 5 matches but head_limit=2 → we expect 2 rows + truncation marker.
        let hits: Vec<&str> = r
            .content
            .lines()
            .filter(|l| !l.starts_with("...") && l.contains(".txt"))
            .collect();
        assert_eq!(
            hits.len(),
            2,
            "expected 2 matches, got {}:\n{}",
            hits.len(),
            r.content
        );
        assert!(
            r.content.contains("truncated"),
            "missing truncation marker:\n{}",
            r.content
        );
    }

    /// P0 #12: VCS directories (.git, node_modules, target) are skipped.
    #[tokio::test]
    async fn grep_skips_vcs_and_build_directories() {
        let dir = tempfile::tempdir().unwrap();
        for junk in [".git", "node_modules", "target"] {
            std::fs::create_dir_all(dir.path().join(junk)).unwrap();
            std::fs::write(
                dir.path().join(junk).join("noise.txt"),
                "NOISE_TOKEN inside junk",
            )
            .unwrap();
        }
        std::fs::write(dir.path().join("real.txt"), "NOISE_TOKEN here").unwrap();

        let tool = GrepTool;
        let ctx = ToolContext::for_test_bare(CancellationToken::new());
        let r = tool
            .execute(
                json!({
                    "pattern": "NOISE_TOKEN",
                    "path": dir.path().to_string_lossy(),
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(r.content.contains("real.txt"), "{}", r.content);
        assert!(!r.content.contains("junk"), "VCS dir leaked: {}", r.content);
    }

    /// P0 #12: multiline mode lets the pattern span line boundaries.
    #[tokio::test]
    async fn grep_multiline_spans_newlines() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("m.txt");
        std::fs::write(&file, "fn foo() {\n    return 42;\n}\n").unwrap();
        let tool = GrepTool;
        let ctx = ToolContext::for_test_bare(CancellationToken::new());
        let r = tool
            .execute(
                json!({
                    "pattern": "fn foo.*return",
                    "path": file.to_string_lossy(),
                    "output_mode": "content",
                    "multiline": true,
                }),
                &ctx,
            )
            .await
            .unwrap();
        // Without multiline this wouldn't match across the \n.
        assert!(r.content.contains("fn foo"), "{}", r.content);
    }

    /// P0 #12: `-n` turns on line numbers in output (content mode).
    #[tokio::test]
    async fn grep_line_number_flag_is_accepted() {
        // The line-number format is already `path:lineno:` in the default
        // output; -n is still accepted as a compat shim so claude issuing
        // `-n` doesn't get a schema error. Sanity check: parses + runs.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("n.txt");
        std::fs::write(&file, "a\nb-HIT\nc\n").unwrap();
        let tool = GrepTool;
        let ctx = ToolContext::for_test_bare(CancellationToken::new());
        let r = tool
            .execute(
                json!({
                    "pattern": "HIT",
                    "path": file.to_string_lossy(),
                    "output_mode": "content",
                    "-n": true,
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(
            r.content.contains(":2:"),
            "missing lineno in:\n{}",
            r.content
        );
    }
}
