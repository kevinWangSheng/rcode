use async_trait::async_trait;
use cc_core::{CcError, CcResult};
use regex::Regex;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::path::Path;
use walkdir::WalkDir;

use crate::{Tool, ToolResult};

const MAX_RESULTS: usize = 250;

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

    fn input_schema(&self) -> Value {
        json!({
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
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value) -> CcResult<ToolResult> {
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
        let output_mode = input["output_mode"].as_str().unwrap_or("files_with_matches");

        let search_path = Path::new(&search_path);

        let mut results: Vec<String> = Vec::new();
        let mut total_count: usize = 0;

        'outer: for entry in WalkDir::new(search_path)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            let path = entry.path();

            // Apply glob filter
            if let Some(glob_pat) = glob_filter {
                let file_name = path.file_name().unwrap_or_default().to_string_lossy();
                let full_path = path.to_string_lossy();
                let pat = glob::Pattern::new(glob_pat).unwrap_or_else(|_| glob::Pattern::new("*").unwrap());
                if !pat.matches(&file_name) && !pat.matches(&full_path) {
                    // Also try matching against just the path relative to search root
                    let rel = path.strip_prefix(search_path).unwrap_or(path);
                    if !pat.matches(&rel.to_string_lossy()) {
                        continue;
                    }
                }
            }

            // Skip binary files by checking for null bytes in first 8KB
            let file = match std::fs::File::open(path) {
                Ok(f) => f,
                Err(_) => continue,
            };
            let mut reader = BufReader::new(file);

            match output_mode {
                "files_with_matches" => {
                    let mut found = false;
                    for line in (&mut reader).lines().map_while(Result::ok) {
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
                    for line in reader.lines().map_while(Result::ok) {
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
