//! Per-path safety checks used by Edit / Write `check_permissions`
//! to enforce bypass-immune asks on paths users almost never mean to
//! let an LLM rewrite — `.git/`, `.claude/`, shell configs, etc.
//! Mirrors TS `safetyCheck` decisions in `src/tools/FileEditTool` and
//! `FileWriteTool`.

use cc_core::{PermissionDecisionReason, PermissionResult, PermissionSource};

/// Path segments that flip an Edit/Write into a bypass-immune Ask.
/// Matched on `path.contains(segment)` so nested forms like
/// `/Users/.../my-repo/.git/config` also trip the check.
const PROTECTED_SEGMENTS: &[&str] = &[
    "/.git/",
    "/.claude/",
    "/.vscode/",
    "/.idea/",
    "/.github/workflows/",
];

/// Filenames whose presence anywhere on the path implies "this is a
/// shell init / runtime config the user manages by hand". An Edit /
/// Write here must ask even in bypass mode — clobbering `.zshrc` on
/// a typo is roughly as bad as `rm -rf ~`.
const PROTECTED_FILENAMES: &[&str] = &[
    ".bashrc",
    ".bash_profile",
    ".zshrc",
    ".zprofile",
    ".profile",
    ".envrc",
    ".npmrc",
    ".pypirc",
    "id_rsa",
    "id_ed25519",
    "id_dsa",
    "authorized_keys",
    "known_hosts",
];

/// Inspect `path` and return `Some(safetyCheck_ask)` when the
/// tool must veto the write regardless of bypass mode. `None`
/// means "no opinion, let the engine decide normally".
pub fn safety_check_path(path: &str) -> Option<PermissionResult> {
    // Normalise leading-slash matching: prepend a `/` so a relative
    // path like `.git/config` still trips `"/.git/"`.
    let normalised = format!("/{}", path.trim_start_matches('/'));

    if let Some(segment) = PROTECTED_SEGMENTS
        .iter()
        .find(|seg| normalised.contains(*seg))
    {
        return Some(
            PermissionResult::deny(
                PermissionSource::SafetyCheck,
                format!("{path} is inside the protected segment '{segment}' — refusing the write"),
            )
            .with_decision_reason(PermissionDecisionReason::SafetyCheck {
                path: path.to_string(),
            })
            .interrupting(),
        );
    }

    let filename = std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    if !filename.is_empty() && PROTECTED_FILENAMES.contains(&filename) {
        return Some(
            PermissionResult::deny(
                PermissionSource::SafetyCheck,
                format!(
                    "{path} is on the protected-filenames list (shell / SSH config); \
                     refusing the write"
                ),
            )
            .with_decision_reason(PermissionDecisionReason::SafetyCheck {
                path: path.to_string(),
            })
            .interrupting(),
        );
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use cc_core::PermissionBehavior;

    #[test]
    fn git_dir_path_is_refused() {
        let r = safety_check_path("/Users/me/repo/.git/config").unwrap();
        assert_eq!(r.behavior, PermissionBehavior::Deny);
        assert!(r.interrupt);
        assert_eq!(r.source, PermissionSource::SafetyCheck);
    }

    #[test]
    fn claude_dir_path_is_refused() {
        let r = safety_check_path("/tmp/proj/.claude/settings.json").unwrap();
        assert_eq!(r.behavior, PermissionBehavior::Deny);
    }

    #[test]
    fn relative_dotgit_path_is_refused() {
        // Relative ".git/foo" also tripped if the leading-slash
        // normalisation works.
        let r = safety_check_path(".git/HEAD").unwrap();
        assert_eq!(r.behavior, PermissionBehavior::Deny);
    }

    #[test]
    fn shell_config_filename_is_refused() {
        for path in [
            "/Users/me/.zshrc",
            "/home/user/.bashrc",
            "/tmp/.envrc",
            "/Users/me/.ssh/id_ed25519",
        ] {
            assert!(
                safety_check_path(path).is_some(),
                "{path} should be refused"
            );
        }
    }

    #[test]
    fn ordinary_paths_are_allowed_to_fall_through() {
        for path in [
            "/Users/me/repo/src/main.rs",
            "/tmp/notes.txt",
            "Cargo.toml",
            "src/lib.rs",
        ] {
            assert!(safety_check_path(path).is_none(), "{path}");
        }
    }
}
