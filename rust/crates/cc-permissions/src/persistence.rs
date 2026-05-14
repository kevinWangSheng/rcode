//! Settings.json persistence for "Allow always" permission rules.
//!
//! When a user picks `AllowAlways` in the TUI permission dialog, the
//! engine flips an in-memory session allow but ALSO needs to write
//! the rule to a settings.json layer so a restart still trusts it
//! (P0 #15 of the 2026-04-24 parity-gaps roadmap). This module owns
//! the on-disk shape.
//!
//! The layout mirrors `cc-config::PermissionsConfig`:
//!
//! ```jsonc
//! {
//!   "permissions": {
//!     "allow": ["Bash", "Edit(*)", "Bash(git push *)"]
//!   }
//! }
//! ```
//!
//! `persist_allow_rule` opens `path`, parses with `serde_json::Value`
//! so unknown fields survive verbatim, walks `permissions.allow`
//! (creating the keys if missing), appends `rule` if it isn't already
//! present, and atomically writes the result back via `tempfile` +
//! `persist` (same pattern as session JSONL). Returns the absolute
//! path of the file we wrote, useful for log lines + tests.
//!
//! Idempotency: appending the same rule twice is a no-op. The cmp
//! is JSON-value equality, not string-equality, so structured rules
//! (`{"tool":"Bash","input":...}`) round-trip cleanly.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use cc_core::{CcError, CcResult};
use serde_json::Value;

/// Append `rule` (a string pattern like `"Bash"` or `"Edit(*)"`) to
/// `permissions.allow` in `settings_path`, writing atomically.
///
/// Returns `Ok(path)` on success — same path passed in, returned so
/// callers can `info!("persisted allow rule to {path:?}")` without
/// re-cloning the input. The file is created if it doesn't exist.
pub fn persist_allow_rule(settings_path: &Path, rule: &str) -> CcResult<PathBuf> {
    persist_rules(settings_path, std::slice::from_ref(&rule.to_string()))
}

/// Bulk variant: append multiple rules in one read-modify-write cycle.
/// Duplicates (matched against existing entries with JSON-value
/// equality) are skipped.
pub fn persist_rules(settings_path: &Path, rules: &[String]) -> CcResult<PathBuf> {
    if let Some(parent) = settings_path.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent).map_err(|e| {
                CcError::io(format!(
                    "failed to create settings parent dir {parent:?}: {e}"
                ))
            })?;
        }
    }

    let mut root = read_root(settings_path)?;
    let allow = ensure_allow_array(&mut root);

    let mut appended = 0usize;
    for rule in rules {
        let needle = Value::String(rule.clone());
        if !allow.iter().any(|existing| existing == &needle) {
            allow.push(needle);
            appended += 1;
        }
    }

    if appended == 0 {
        // Nothing to do; don't even rewrite the file. Saves a write
        // (and an mtime bump) when the user re-confirms a rule the
        // session already loaded from disk.
        return Ok(settings_path.to_path_buf());
    }

    write_atomically(settings_path, &root)?;
    tracing::info!(
        path = %settings_path.display(),
        added = appended,
        "persisted {appended} allow rule(s)"
    );
    Ok(settings_path.to_path_buf())
}

/// Open `path` and return the parsed JSON root. An absent file is
/// treated as `{}` so the very first AllowAlways still works on a
/// fresh install. A malformed file surfaces as a `CcError::Config`
/// — we'd rather fail loud than overwrite the user's hand-edits.
fn read_root(path: &Path) -> CcResult<Value> {
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| {
            CcError::Config(format!("settings.json at {path:?} is not valid JSON: {e}"))
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Value::Object(Default::default())),
        Err(e) => Err(CcError::io(format!(
            "failed to read settings.json at {path:?}: {e}"
        ))),
    }
}

/// Borrow the `permissions.allow` array out of `root`, creating the
/// intermediate keys as needed. Returns the array's `&mut Vec<Value>`
/// so the caller can push to it directly.
fn ensure_allow_array(root: &mut Value) -> &mut Vec<Value> {
    if !root.is_object() {
        *root = Value::Object(Default::default());
    }
    let permissions = root
        .as_object_mut()
        .unwrap()
        .entry("permissions")
        .or_insert_with(|| Value::Object(Default::default()));
    if !permissions.is_object() {
        *permissions = Value::Object(Default::default());
    }
    let allow = permissions
        .as_object_mut()
        .unwrap()
        .entry("allow")
        .or_insert_with(|| Value::Array(Vec::new()));
    if !allow.is_array() {
        *allow = Value::Array(Vec::new());
    }
    allow.as_array_mut().unwrap()
}

/// Write `value` to `path` atomically via `NamedTempFile` + `persist`,
/// so a crash mid-write can never leave a half-written settings.json.
fn write_atomically(path: &Path, value: &Value) -> CcResult<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(parent)
        .map_err(|e| CcError::io(format!("failed to create temp file in {parent:?}: {e}")))?;
    let text = serde_json::to_string_pretty(value)
        .map_err(|e| CcError::io(format!("failed to serialise settings.json: {e}")))?;
    tmp.write_all(text.as_bytes())
        .map_err(|e| CcError::io(format!("failed to write temp file: {e}")))?;
    tmp.write_all(b"\n")
        .map_err(|e| CcError::io(format!("failed to write temp file trailer: {e}")))?;
    tmp.persist(path)
        .map_err(|e| CcError::io(format!("failed to persist settings.json: {e}")))?;
    Ok(())
}

/// Best-effort path for the user-level settings.json
/// (`~/.claude/settings.json`). Mirrors
/// `cc_config::ConfigPaths::global_settings()` but reimplemented here
/// so cc-permissions doesn't need to take a dep on cc-config (which
/// would introduce a cycle with future "permissions ↔ config"
/// integration). Callers that already have a path in hand should
/// just pass it through `persist_allow_rule` directly.
pub fn default_user_settings_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("settings.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_to_missing_file_creates_settings_with_allow() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        persist_allow_rule(&path, "Bash").unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        let parsed: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed["permissions"]["allow"][0], "Bash");
    }

    #[test]
    fn append_preserves_unknown_top_level_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        // Hand-rolled settings with a field cc-permissions doesn't know.
        fs::write(
            &path,
            r#"{"theme": "dark", "permissions": {"allow": ["Read"]}}"#,
        )
        .unwrap();
        persist_allow_rule(&path, "Bash").unwrap();
        let parsed: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed["theme"], "dark", "unknown key dropped");
        let allow = parsed["permissions"]["allow"].as_array().unwrap();
        assert_eq!(allow.len(), 2);
        assert_eq!(allow[0], "Read");
        assert_eq!(allow[1], "Bash");
    }

    #[test]
    fn duplicate_rules_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        persist_allow_rule(&path, "Bash").unwrap();
        persist_allow_rule(&path, "Bash").unwrap();
        persist_allow_rule(&path, "Bash").unwrap();
        let parsed: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let allow = parsed["permissions"]["allow"].as_array().unwrap();
        assert_eq!(allow.len(), 1, "duplicate persist should be a no-op");
    }

    #[test]
    fn malformed_settings_surfaces_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, "{not json").unwrap();
        let err = persist_allow_rule(&path, "Bash").unwrap_err().to_string();
        assert!(err.contains("not valid JSON"), "got: {err}");
    }

    #[test]
    fn permissions_block_with_wrong_type_gets_replaced() {
        // Defensive: if `permissions` was hand-set to a string (typo),
        // we replace it with the canonical object shape rather than
        // crashing. Same for an `allow` field that's not an array.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, r#"{"permissions": "broken"}"#).unwrap();
        persist_allow_rule(&path, "Bash").unwrap();
        let parsed: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(parsed["permissions"].is_object());
        let allow = parsed["permissions"]["allow"].as_array().unwrap();
        assert_eq!(allow[0], "Bash");
    }

    #[test]
    fn bulk_persist_appends_all_new_dedupes_existing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        persist_allow_rule(&path, "Read").unwrap();
        persist_rules(&path, &["Read".into(), "Bash".into(), "Edit(*)".into()]).unwrap();
        let parsed: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let allow = parsed["permissions"]["allow"].as_array().unwrap();
        let strs: Vec<&str> = allow.iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(strs, vec!["Read", "Bash", "Edit(*)"]);
    }
}
