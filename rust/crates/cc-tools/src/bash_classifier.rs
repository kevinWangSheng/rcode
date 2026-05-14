//! Bash command classifier — splits a command line into a permission
//! opinion (allow / deny / ask) without spawning a shell. Ported from
//! TS `src/tools/BashTool/bashClassifier.ts` + `dangerousPatterns.ts`.
//!
//! The classifier is intentionally narrow: it only handles patterns
//! that are unambiguously read-only (so the engine can skip the
//! permission dialog) or unambiguously dangerous (so the engine
//! refuses regardless of session allow rules). Anything in between
//! returns `None` and falls through to the settings-level rules.
//!
//! Calling pattern:
//! ```ignore
//! use cc_tools::bash_classifier::classify;
//! match classify("ls -la") {
//!     Some(result) => /* tool has an opinion */,
//!     None => /* fall through to settings */,
//! }
//! ```

use cc_core::{PermissionDecisionReason, PermissionResult, PermissionSource};

/// Subcommands that are always read-only on a typical Unix box. Calls
/// with these as the first non-redirection token bypass the
/// permission dialog (matches TS `READ_ONLY_BASH_COMMANDS`). Keep
/// this list narrow — every entry trades a prompt for a class of
/// potentially-mutating side effect (e.g. `find` with `-delete`).
const READ_ONLY_COMMANDS: &[&str] = &[
    "ls", "cat", "head", "tail", "less", "more", "pwd", "echo", "printf", "wc", "grep", "egrep",
    "fgrep", "rg", "ripgrep", "find", "stat", "file", "tree", "du", "df", "which", "type",
    "command", "whoami", "hostname", "uname", "date", "true", "false", "test", "[", "env",
];

/// Patterns that should never run. Matched against the FULL command
/// string with a substring check — these strings have no legitimate
/// reading in a Claude Code session. Mirrors TS `DANGEROUS_PATTERNS`.
const DANGEROUS_PATTERNS: &[&str] = &[
    "rm -rf /",
    "rm -rf ~",
    "rm -rf $HOME",
    "rm -rf .",
    "rm -rf *",
    "mkfs",
    "dd if=/dev/zero",
    "dd if=/dev/random",
    ":(){ :|:& };:", // fork bomb
    "shutdown",
    "reboot",
    "init 0",
    "init 6",
    "halt",
    "poweroff",
];

/// Classify a Bash command. Returns:
/// - `Some(allow)` for read-only, safe subcommands.
/// - `Some(deny)` for entries on `DANGEROUS_PATTERNS`.
/// - `None` when the classifier has no opinion (fall through).
pub fn classify(command: &str) -> Option<PermissionResult> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Dangerous patterns short-circuit — a session allow rule should
    // NOT be able to override these. The engine respects that by
    // checking tool-opinion first (Change B of the permission refactor).
    for needle in DANGEROUS_PATTERNS {
        if trimmed.contains(needle) {
            return Some(
                PermissionResult::deny(
                    PermissionSource::ToolCheck,
                    format!("Bash refuses '{needle}': command is on the dangerous-patterns list"),
                )
                .with_decision_reason(PermissionDecisionReason::SafetyCheck {
                    path: command.to_string(),
                })
                .interrupting(),
            );
        }
    }

    // Strip leading env / variable assignments and pipe stages — only
    // look at the FIRST executed command. `FOO=1 BAR=2 ls -la` should
    // still classify as read-only `ls`. `ls | grep foo` is read-only
    // iff every pipe stage is read-only.
    let pipe_stages: Vec<&str> = trimmed.split('|').collect();
    let all_read_only = pipe_stages.iter().all(|stage| {
        let stage = stage.trim();
        let head = first_command_token(stage);
        head.is_some_and(|h| READ_ONLY_COMMANDS.contains(&h))
    });
    if all_read_only && !pipe_stages.is_empty() {
        return Some(
            PermissionResult::allow(PermissionSource::ToolCheck).with_decision_reason(
                PermissionDecisionReason::Other(format!(
                    "Bash classifier: read-only subcommand(s) in '{}'",
                    trimmed
                )),
            ),
        );
    }

    None
}

/// Return the first non-assignment token of a command stage. Skips
/// `FOO=1` style env prefixes. Returns `None` if the stage is empty.
fn first_command_token(stage: &str) -> Option<&str> {
    for raw in stage.split_whitespace() {
        if raw.contains('=') && !raw.starts_with('"') && !raw.starts_with('\'') {
            // Looks like FOO=bar; skip.
            continue;
        }
        return Some(raw);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use cc_core::PermissionBehavior;

    #[test]
    fn read_only_commands_short_circuit_to_allow() {
        for cmd in ["ls", "ls -la", "cat /etc/hosts", "rg --files", "echo hi"] {
            let r = classify(cmd).unwrap_or_else(|| panic!("classifier returned None for {cmd}"));
            assert_eq!(r.behavior, PermissionBehavior::Allow, "{cmd} → {r:?}");
            assert_eq!(r.source, PermissionSource::ToolCheck);
        }
    }

    #[test]
    fn env_prefix_does_not_confuse_classifier() {
        let r = classify("FOO=bar BAZ=qux ls -la").unwrap();
        assert_eq!(r.behavior, PermissionBehavior::Allow);
    }

    #[test]
    fn pipe_of_read_only_commands_is_read_only() {
        let r = classify("ls -la | grep foo | wc -l").unwrap();
        assert_eq!(r.behavior, PermissionBehavior::Allow);
    }

    #[test]
    fn pipe_with_any_unknown_stage_is_none() {
        // `awk` isn't in the read-only allowlist; mixed pipe should
        // fall through to the settings rules instead of auto-allowing.
        assert!(classify("ls | awk '{print $1}'").is_none());
    }

    #[test]
    fn unknown_commands_return_none() {
        for cmd in ["go build", "npm install", "make", "git push"] {
            assert!(
                classify(cmd).is_none(),
                "{cmd} should fall through, got {:?}",
                classify(cmd)
            );
        }
    }

    #[test]
    fn dangerous_patterns_are_denied_and_interrupting() {
        for cmd in [
            "rm -rf /",
            "rm -rf $HOME",
            "dd if=/dev/zero of=/dev/sda",
            "mkfs.ext4 /dev/sda1",
        ] {
            let r = classify(cmd).unwrap_or_else(|| panic!("classifier returned None for {cmd}"));
            assert_eq!(r.behavior, PermissionBehavior::Deny, "{cmd} → {r:?}");
            assert!(r.interrupt, "dangerous patterns must interrupt the turn");
        }
    }

    #[test]
    fn empty_command_returns_none() {
        assert!(classify("").is_none());
        assert!(classify("   ").is_none());
    }
}
