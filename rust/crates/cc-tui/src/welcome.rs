//! Welcome banner — rendered into the transcript viewport when no messages
//! have been exchanged yet (Phase D3).
//!
//! Visual contract:
//!
//! ```text
//! Welcome to Claude Code v0.1.0
//!
//!      ╭──────────────────────────╮
//!      │ ▒▒▒▒▒▒▒▒▒▒▒▒  █████████  │
//!      │ ▒▒▒▒▒▒▒▒▒▒▒▒ ██▄█████▄██ │
//!      │ ▒▒▒▒▒▒▒▒▒▒▒▒  █████████  │
//!      │ ▒▒▒▒▒▒▒▒▒▒▒▒  █ █   █ █  │
//!      ╰──────────────────────────╯
//!
//!  cwd: ~/dev/project
//!  Tip: Run /help to see commands
//! ```
//!
//! On terminals narrower than [`MIN_FANCY_WIDTH`] the banner falls back to a
//! one-line `✻ Claude Code v0.1.0` so it never wraps mid-art.
//!
//! The tip rotates deterministically off the session-start instant so the
//! same tip persists for ~30 s windows without depending on a live timer.

use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

use crate::theme;

/// Below this terminal width the banner downgrades to a single line. The
/// fancy art targets a 60-char box; clipping that mid-glyph looks broken.
pub const MIN_FANCY_WIDTH: u16 = 60;

/// Curated tip set, kept short enough to render on a single 60-char row.
/// Order is stable so tests can assert at least one substring is present.
pub const TIPS: &[&str] = &[
    "Run /help to see all slash commands",
    "@-mention a file path to attach it to your prompt",
    "Type ! to drop into a one-shot bash command",
    "Press Ctrl+C twice to force-quit a stuck turn",
    "Use Shift+Up/Down to scroll the transcript",
    "Custom keybindings live in ~/.claude/keybindings.json",
    "/model swaps between Opus, Sonnet, and Haiku",
    "/cost shows accumulated token usage and dollar estimate",
    "/compact summarises the conversation when context fills up",
    "Skills under ~/.claude/skills are auto-discovered as commands",
    "Set CC_TUI_MINIMAL=1 to disable markdown + tool cards",
    "/reload-keybindings re-reads your shortcuts without restart",
];

/// Build the welcome banner as a list of styled lines, suitable for
/// pushing into the transcript viewport. Width drives layout: wider than
/// [`MIN_FANCY_WIDTH`] gets the boxed clawd; narrower gets the one-liner.
pub fn render_welcome(width: u16, version: &str, cwd: &str, tip_seed: u64) -> Vec<Line<'static>> {
    let theme = theme::current();
    let tip = TIPS[(tip_seed as usize) % TIPS.len()];

    if width < MIN_FANCY_WIDTH {
        return vec![
            Line::from(vec![
                Span::styled(
                    "✻ Claude Code ",
                    Style::default()
                        .fg(theme.claude_orange)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("v{version}"), Style::default().fg(theme.dim)),
            ]),
            Line::from(Span::styled(
                format!(" cwd: {cwd}"),
                Style::default().fg(theme.dim),
            )),
            Line::from(Span::styled(
                format!(" Tip: {tip}"),
                Style::default().fg(theme.subtle),
            )),
            Line::from(""),
        ];
    }

    let mut out: Vec<Line<'static>> = Vec::with_capacity(16);
    out.push(Line::from(vec![
        Span::styled(
            "Welcome to Claude Code ",
            Style::default()
                .fg(theme.claude_orange)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("v{version}"), Style::default().fg(theme.dim)),
    ]));
    out.push(Line::from(""));

    // Boxed condensed clawd. Box width is fixed at 28 chars (inner) +
    // borders to fit comfortably under MIN_FANCY_WIDTH.
    let pad = "     ";
    out.push(Line::from(Span::styled(
        format!("{pad}╭──────────────────────────╮"),
        Style::default().fg(theme.subtle),
    )));
    for body in CLAWD_BODY {
        out.push(Line::from(vec![
            Span::styled(format!("{pad}│ "), Style::default().fg(theme.subtle)),
            Span::styled(
                (*body).to_string(),
                Style::default().fg(theme.claude_orange),
            ),
            Span::styled(" │", Style::default().fg(theme.subtle)),
        ]));
    }
    out.push(Line::from(Span::styled(
        format!("{pad}╰──────────────────────────╯"),
        Style::default().fg(theme.subtle),
    )));
    out.push(Line::from(""));

    out.push(Line::from(vec![
        Span::styled(" cwd: ", Style::default().fg(theme.dim)),
        Span::styled(cwd.to_string(), Style::default().fg(theme.text)),
    ]));
    out.push(Line::from(vec![
        Span::styled(" Tip: ", Style::default().fg(theme.dim)),
        Span::styled(tip.to_string(), Style::default().fg(theme.subtle)),
    ]));
    out.push(Line::from(""));
    out
}

/// Compact ASCII-art clawd inspired by the dark-theme branch of
/// `WelcomeV2.tsx` (lines 80–104). The official art is 58 chars wide and
/// includes a decorative ellipsis frame; we strip the frame and pad the
/// figure to fit a 26-char inner box.
const CLAWD_BODY: &[&str] = &[
    "  ▒▒▒▒▒▒▒▒   █████████  ",
    " ▒▒▒▒▒▒▒▒▒▒ ██▄█████▄██ ",
    " ▒▒▒▒▒▒▒▒▒▒  █████████  ",
    "             █ █   █ █  ",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn line_to_plain(l: &Line<'_>) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn fancy_banner_includes_version_cwd_and_tip() {
        let lines = render_welcome(80, "0.1.0", "~/dev/cc-rust", 0);
        let joined: String = lines
            .iter()
            .map(line_to_plain)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("Welcome to Claude Code"), "{joined}");
        assert!(joined.contains("v0.1.0"), "{joined}");
        assert!(joined.contains("cwd: ~/dev/cc-rust"), "{joined}");
        // First tip from the rotation.
        assert!(joined.contains(TIPS[0]), "{joined}");
    }

    #[test]
    fn tip_seed_rotates_deterministically() {
        let n = TIPS.len() as u64;
        for i in 0..n * 2 {
            let lines = render_welcome(80, "0.1.0", "/", i);
            let joined: String = lines
                .iter()
                .map(line_to_plain)
                .collect::<Vec<_>>()
                .join("\n");
            let expected = TIPS[(i as usize) % TIPS.len()];
            assert!(
                joined.contains(expected),
                "seed {i} should pick tip {expected}; got: {joined}"
            );
        }
    }

    #[test]
    fn narrow_banner_falls_back_to_one_liner() {
        let lines = render_welcome(40, "0.1.0", "/tmp", 0);
        let joined: String = lines
            .iter()
            .map(line_to_plain)
            .collect::<Vec<_>>()
            .join("\n");
        // No box-drawing characters in the compact path.
        assert!(
            !joined.contains("╭"),
            "fallback should not draw a box: {joined}"
        );
        assert!(joined.contains("Claude Code"), "{joined}");
        assert!(joined.contains("Tip:"), "{joined}");
    }
}
