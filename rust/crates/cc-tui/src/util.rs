//! UTF-8-safe truncation + JSON input summarisation helpers shared by the
//! engine-event mapper and any future preview-style renderer.
//!
//! Extracted from `lib.rs` (2026-04-20 refactor). No behavior change — pure
//! byte-level helpers with no terminal side effects.

/// Return the longest prefix of `s` that fits in `max_bytes` **and** ends on
/// a UTF-8 char boundary. Using `&s[..max_bytes]` directly panics if byte
/// `max_bytes` lands inside a multi-byte codepoint — very common with CJK
/// (3 bytes/char) and emoji (4 bytes/char). Because the renderer runs under
/// crossterm raw mode, such a panic scrambles the terminal and the stderr
/// message is swallowed, making the TUI look like it "just died after a few
/// messages". This helper is the canonical truncation point for all
/// byte-bounded previews in this crate.
pub(crate) fn truncate_at_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Format a JSON `Value` as a short one-line summary for the transcript.
///
/// Field-priority list mirrors the names used by the actual tool schemas
/// in `cc-tools/src/`:
///   - `command`           — Bash
///   - `file_path`         — Edit, Write, MultiEdit, Read
///   - `path`              — Glob (and any future tool that prefers `path`)
///   - `pattern`           — Grep, Glob
///   - `query`             — WebSearch
///   - `url`               — WebFetch
///   - `content`           — Write
///   - `prompt`            — TaskCreate, ApiSummarizer
///   - `name` / `title`    — TaskUpdate / generic
///
/// Adding `file_path` here was the user-visible fix for "Edit tool card
/// shows raw JSON" surfaced by the npcterm-driven smoke test. Without it,
/// every Edit/Write call rendered as `Edit({"file_path":"…","old_string":
/// "…",…})` instead of the cleaner `Edit(src/main.rs)`.
pub(crate) fn summarize_input(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Object(map) => {
            for key in &[
                "command",
                "file_path",
                "path",
                "pattern",
                "query",
                "url",
                "content",
                "prompt",
                "name",
                "title",
            ] {
                if let Some(serde_json::Value::String(s)) = map.get(*key) {
                    let s = s.trim();
                    if s.len() > 120 {
                        return format!("{}…", truncate_at_char_boundary(s, 120));
                    }
                    return s.to_string();
                }
            }
            // Fall back to compact JSON, truncated.
            let s = serde_json::to_string(v).unwrap_or_default();
            if s.len() > 120 {
                format!("{}…", truncate_at_char_boundary(&s, 120))
            } else {
                s
            }
        }
        serde_json::Value::String(s) => {
            if s.len() > 120 {
                format!("{}…", truncate_at_char_boundary(s, 120))
            } else {
                s.clone()
            }
        }
        other => {
            let s = other.to_string();
            if s.len() > 120 {
                format!("{}…", truncate_at_char_boundary(&s, 120))
            } else {
                s
            }
        }
    }
}

/// Truncate long tool output to a preview for transcript display.
pub(crate) fn truncate_output(s: &str) -> String {
    const MAX_LINES: usize = 20;
    const MAX_BYTES: usize = 2000;
    let lines: Vec<&str> = s.lines().take(MAX_LINES + 1).collect();
    let truncated_lines = lines.len() > MAX_LINES;
    let joined = lines[..lines.len().min(MAX_LINES)].join("\n");
    if truncated_lines || joined.len() > MAX_BYTES {
        let preview = truncate_at_char_boundary(&joined, MAX_BYTES);
        format!("{preview}\n… (truncated)")
    } else {
        joined
    }
}

#[cfg(test)]
mod tests {
    //! The TUI used to run `&s[..120]` / `&joined[..2000]` byte slicing on
    //! tool input summaries and tool output previews. A single Chinese/emoji
    //! character at the exact cutoff byte turned into a panic under
    //! crossterm raw mode, which is why chat sessions with CJK content
    //! "just died". These tests anchor the boundary-safe helpers so a
    //! future refactor can't reintroduce the panic silently.
    use super::*;
    use serde_json::json;

    #[test]
    fn truncate_at_char_boundary_never_splits_codepoints() {
        // 41 CJK chars × 3 bytes = 123 bytes — byte 120 lands inside the
        // 41st char. Direct slicing would panic.
        let s: String = "我".repeat(41);
        assert_eq!(s.len(), 123);
        let truncated = truncate_at_char_boundary(&s, 120);
        // 120 / 3 = 40 chars, 120 bytes exactly is a boundary.
        assert_eq!(truncated.chars().count(), 40);
        assert_eq!(truncated.len() % 3, 0);
    }

    #[test]
    fn truncate_at_char_boundary_backs_off_partial_codepoints() {
        // One 4-byte emoji: if we try to cut at 2 bytes we must back off to 0.
        let s = "🎉abc"; // emoji is 4 bytes
        let cut = truncate_at_char_boundary(s, 2);
        assert_eq!(cut, ""); // must back off before the emoji
        let cut4 = truncate_at_char_boundary(s, 4);
        assert_eq!(cut4, "🎉"); // exact boundary
        let cut5 = truncate_at_char_boundary(s, 5);
        assert_eq!(cut5, "🎉a"); // one ASCII after the emoji
    }

    #[test]
    fn truncate_at_char_boundary_returns_full_string_when_short() {
        assert_eq!(truncate_at_char_boundary("hi", 100), "hi");
        assert_eq!(truncate_at_char_boundary("", 100), "");
    }

    /// Regression: a long Chinese bash command used to crash the TUI the
    /// moment it arrived as a tool-start event.
    #[test]
    fn summarize_long_chinese_command_does_not_panic() {
        let long = "echo ".to_string() + &"测试中文命令超过一百二十字节的情况".repeat(10);
        // This would panic on the old byte-slicing path.
        let s = summarize_input(&json!({ "command": long }));
        assert!(s.ends_with("…"), "expected ellipsis suffix, got {s:?}");
    }

    /// Regression: tool output filled with emoji used to crash on the
    /// 2 KiB truncate path.
    #[test]
    fn truncate_output_with_emoji_does_not_panic() {
        let huge = "🎉".repeat(800); // ~3.2 KiB — above MAX_BYTES (2000)
        let s = truncate_output(&huge);
        assert!(s.contains("… (truncated)"), "got {s:?}");
    }

    /// Pure ASCII path is unchanged — a 200-byte command truncates at
    /// exactly 120 bytes + ellipsis.
    #[test]
    fn summarize_long_ascii_command_truncates_to_120_bytes() {
        let long = "a".repeat(200);
        let s = summarize_input(&json!({ "command": long }));
        assert!(s.ends_with("…"));
        // 120 'a's + one '…' (3-byte char).
        assert_eq!(s.chars().count(), 121);
    }

    /// Regression for the npcterm-found bug: Edit/Write/MultiEdit use
    /// `file_path`, not `path`. Pre-fix the summary fell through to a
    /// raw JSON dump, so every Edit tool card looked like
    /// `Edit({"file_path":"…","old_string":"…",…})`.
    #[test]
    fn summarize_edit_uses_file_path_field() {
        let s = summarize_input(&json!({
            "file_path": "src/main.rs",
            "old_string": "hello",
            "new_string": "world"
        }));
        assert_eq!(s, "src/main.rs", "got {s:?}");
    }

    /// `path` (Glob) and `pattern` (Grep) keep working alongside file_path.
    #[test]
    fn summarize_path_and_pattern_still_work() {
        assert_eq!(summarize_input(&json!({ "path": "**/*.rs" })), "**/*.rs");
        assert_eq!(
            summarize_input(&json!({ "pattern": "fn main", "glob": "*.rs" })),
            "fn main"
        );
    }

    /// `prompt` (TaskCreate, ApiSummarizer) is now picked up too.
    #[test]
    fn summarize_picks_up_prompt_field() {
        assert_eq!(
            summarize_input(&json!({ "prompt": "summarise this file", "max_tokens": 200 })),
            "summarise this file"
        );
    }
}
