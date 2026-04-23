//! Feature-gated syntect integration for fenced code-block highlighting.
//!
//! Compiled only when the `tui-syntect` feature is enabled. The module
//! exposes `FenceHighlighter`, a per-fence state object that the
//! markdown renderer constructs at fence-open and drops at fence-close.
//!
//! The `SyntaxSet` and `ThemeSet` are `std::sync::OnceLock` statics so
//! that the ~2 MiB of grammar/theme data is parsed exactly once per
//! process. `HighlightLines` borrows into both, so the instance we hand
//! back carries a `'static` lifetime parameter.

use std::sync::OnceLock;

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{Style as SyntectStyle, Theme, ThemeSet};
use syntect::parsing::SyntaxSet;

static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
static THEME: OnceLock<Theme> = OnceLock::new();

fn syntax_set() -> &'static SyntaxSet {
    SYNTAX_SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn theme() -> &'static Theme {
    THEME.get_or_init(|| {
        let ts = ThemeSet::load_defaults();
        ts.themes
            .get("base16-ocean.dark")
            .cloned()
            .expect("base16-ocean.dark ships with syntect defaults")
    })
}

/// Per-fence highlighter. Constructed at fence-open, fed one line at a
/// time through the fenced block, dropped at fence-close.
pub struct FenceHighlighter {
    inner: HighlightLines<'static>,
}

impl FenceHighlighter {
    /// Attempt to build a highlighter for `lang`. Returns `None` when
    /// the language hint is unknown (e.g. `plaintext`, `diff-ish`,
    /// something typo'd); the caller should fall back to plain rendering.
    pub fn new(lang: &str) -> Option<Self> {
        let set = syntax_set();
        let syntax = set.find_syntax_by_token(lang)?;
        Some(Self {
            inner: HighlightLines::new(syntax, theme()),
        })
    }

    /// Render a single line of code with syntect-derived spans,
    /// prefixed by the fence border gutter so the output matches the
    /// plain `render_code_line` path visually.
    pub fn highlight_line(&mut self, line: &str) -> Line<'static> {
        let ranges = self
            .inner
            .highlight_line(line, syntax_set())
            .unwrap_or_default();
        let border = Span::styled("│ ".to_string(), Style::default().fg(Color::DarkGray));
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(ranges.len() + 1);
        spans.push(border);
        for (style, text) in ranges {
            spans.push(Span::styled(text.to_string(), to_ratatui_style(style)));
        }
        Line::from(spans)
    }
}

fn to_ratatui_style(s: SyntectStyle) -> Style {
    let fg = s.foreground;
    Style::default().fg(Color::Rgb(fg.r, fg.g, fg.b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_language_returns_none() {
        assert!(FenceHighlighter::new("totally-not-a-language").is_none());
    }

    #[test]
    fn rust_keyword_gets_non_default_color() {
        let mut h = FenceHighlighter::new("rust").expect("rust syntax ships with defaults");
        let line = h.highlight_line("fn main() {}");
        // Gutter + at least one highlighted span.
        assert!(line.spans.len() >= 2, "expected gutter + >=1 code span");
        // `fn` should carry an explicit foreground — not the terminal default.
        let fn_span = line
            .spans
            .iter()
            .find(|s| s.content.contains("fn"))
            .expect("fn span missing");
        assert!(
            matches!(fn_span.style.fg, Some(Color::Rgb(_, _, _))),
            "syntect should set an Rgb foreground on the `fn` keyword"
        );
    }
}
