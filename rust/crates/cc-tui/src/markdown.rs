//! Minimal markdown renderer — converts a markdown-ish string into a
//! `Vec<Line>` for the ratatui transcript.
//!
//! Scope is intentionally limited to what assistant replies actually
//! emit (per M5 AC-V1):
//!
//!   - Inline: `**bold**`, `*italic*`, `` `code` ``, `[label](url)`
//!   - Block: `#` / `##` / `###` headings, `-` / `*` / `N.` lists,
//!     `>` blockquotes, `---` horizontal rules, ` ``` ` fenced
//!     code blocks (with optional language label on the fence).
//!
//! The parser is single-pass, line-oriented. Unterminated fences are
//! rendered with a trailing `"...streaming"` hint so the user sees
//! something sensible during live streaming.
//!
//! The renderer deliberately avoids a full CommonMark implementation:
//! an 80-line tokenizer is easier to audit than pulling in `pulldown-cmark`.
//! Every inline dialect quirk that bites us gets a regression test in
//! `#[cfg(test)] mod tests` below.

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

/// Render `text` as a sequence of ratatui `Line`s.
///
/// The output is always safe to push into a `Paragraph`; the caller
/// does not need to handle any markdown state itself.
pub fn render_markdown(text: &str) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut in_fence = false;

    for raw in text.split_inclusive('\n') {
        // Strip trailing newline; the Line itself carries the row break.
        let line = raw.strip_suffix('\n').unwrap_or(raw);

        // ── Fenced code block state machine ──────────────────────────
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("```") {
            if !in_fence {
                in_fence = true;
                let lang = rest.trim();
                let fence_lang: Option<String> = (!lang.is_empty()).then(|| lang.to_string());
                out.push(render_fence_open(fence_lang.as_deref()));
            } else {
                in_fence = false;
                out.push(render_fence_close());
            }
            continue;
        }
        if in_fence {
            out.push(render_code_line(line));
            continue;
        }

        out.push(render_block_line(line));
    }

    if in_fence {
        // Unterminated fence: user is still seeing the code stream
        // through. Render a hint so the frame doesn't look broken.
        out.push(Line::from(Span::styled(
            "... streaming".to_string(),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )));
    }

    out
}

fn render_fence_open(lang: Option<&str>) -> Line<'static> {
    let label = match lang {
        Some(l) => format!("┌─ {l} "),
        None => "┌─ code ".to_string(),
    };
    Line::from(Span::styled(label, fence_border_style()))
}

fn render_fence_close() -> Line<'static> {
    Line::from(Span::styled("└─".to_string(), fence_border_style()))
}

fn render_code_line(line: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled("│ ".to_string(), fence_border_style()),
        Span::styled(line.to_string(), code_style()),
    ])
}

fn fence_border_style() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn code_style() -> Style {
    // Reversed-ish look: a subtle background tint keeps code readable
    // without drawing the eye away from prose.
    Style::default().fg(Color::LightYellow)
}

fn render_block_line(line: &str) -> Line<'static> {
    let trimmed = line.trim_start();

    // Horizontal rule: `---` / `***` / `___` (3+ of same char alone on a line).
    if is_horizontal_rule(trimmed) {
        return Line::from(Span::styled(
            "─".repeat(40),
            Style::default().fg(Color::DarkGray),
        ));
    }

    // Heading: `#`/`##`/`###` followed by a space.
    if let Some((level, rest)) = parse_heading(trimmed) {
        let style = heading_style(level);
        let prefix = match level {
            1 => "█ ",
            2 => "▌ ",
            _ => "▎ ",
        };
        let mut spans = vec![Span::styled(prefix.to_string(), style)];
        spans.extend(render_inline(rest, Some(style)));
        return Line::from(spans);
    }

    // Blockquote: `> ...`.
    if let Some(rest) = trimmed.strip_prefix("> ") {
        let mut spans = vec![Span::styled(
            "▏ ".to_string(),
            Style::default().fg(Color::DarkGray),
        )];
        spans.extend(render_inline(
            rest,
            Some(Style::default().fg(Color::DarkGray)),
        ));
        return Line::from(spans);
    }

    // Bulleted list: `-` or `*` or `+` followed by a space.
    if let Some(rest) = parse_bullet(trimmed) {
        let indent = line.len() - trimmed.len();
        let mut spans = vec![Span::raw(" ".repeat(indent))];
        spans.push(Span::styled(
            "• ".to_string(),
            Style::default().fg(Color::Cyan),
        ));
        spans.extend(render_inline(rest, None));
        return Line::from(spans);
    }

    // Numbered list: `1.` / `12.` followed by a space.
    if let Some((num, rest)) = parse_ordered(trimmed) {
        let indent = line.len() - trimmed.len();
        let mut spans = vec![Span::raw(" ".repeat(indent))];
        spans.push(Span::styled(
            format!("{num}. "),
            Style::default().fg(Color::Cyan),
        ));
        spans.extend(render_inline(rest, None));
        return Line::from(spans);
    }

    // Plain paragraph.
    Line::from(render_inline(line, None))
}

fn is_horizontal_rule(s: &str) -> bool {
    if s.len() < 3 {
        return false;
    }
    let first = s.chars().next().unwrap();
    if !matches!(first, '-' | '*' | '_') {
        return false;
    }
    s.chars().all(|c| c == first)
}

fn parse_heading(s: &str) -> Option<(u8, &str)> {
    let mut hashes = 0u8;
    for c in s.chars() {
        if c == '#' && hashes < 6 {
            hashes += 1;
        } else {
            break;
        }
    }
    if hashes == 0 {
        return None;
    }
    let rest = &s[hashes as usize..];
    rest.strip_prefix(' ').map(|r| (hashes, r))
}

fn parse_bullet(s: &str) -> Option<&str> {
    let first = s.chars().next()?;
    if matches!(first, '-' | '*' | '+') {
        s[1..].strip_prefix(' ')
    } else {
        None
    }
}

fn parse_ordered(s: &str) -> Option<(&str, &str)> {
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || digits.len() > 3 {
        return None;
    }
    let rest = &s[digits.len()..];
    let after_dot = rest.strip_prefix('.')?.strip_prefix(' ')?;
    Some((&s[..digits.len()], after_dot))
}

fn heading_style(level: u8) -> Style {
    match level {
        1 => Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD),
        2 => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        _ => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
    }
}

/// Render an inline segment into spans.
///
/// `base` is applied to every emitted span so a heading can forward
/// its bold/color while still letting `**bold**` toggle modifiers.
fn render_inline(text: &str, base: Option<Style>) -> Vec<Span<'static>> {
    let base = base.unwrap_or_default();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0usize;
    let mut buf = String::new();

    let push_plain = |spans: &mut Vec<Span<'static>>, buf: &mut String, base: Style| {
        if !buf.is_empty() {
            spans.push(Span::styled(std::mem::take(buf), base));
        }
    };

    while i < bytes.len() {
        let rest = &text[i..];

        // `code` — backtick-delimited inline code.
        if let Some(body) = rest.strip_prefix('`') {
            if let Some(end) = body.find('`') {
                push_plain(&mut spans, &mut buf, base);
                let code = &body[..end];
                spans.push(Span::styled(
                    code.to_string(),
                    base.patch(code_style()).add_modifier(Modifier::BOLD),
                ));
                i += 1 + end + 1;
                continue;
            }
        }

        // **bold**
        if let Some(body) = rest.strip_prefix("**") {
            if let Some(end) = body.find("**") {
                push_plain(&mut spans, &mut buf, base);
                let inner = &body[..end];
                spans.extend(render_inline(
                    inner,
                    Some(base.add_modifier(Modifier::BOLD)),
                ));
                i += 2 + end + 2;
                continue;
            }
        }

        // *italic* — note we must exclude "**" which has already been tried above.
        if !rest.starts_with("**") {
            if let Some(body) = rest.strip_prefix('*') {
                if let Some(end) = body.find('*') {
                    push_plain(&mut spans, &mut buf, base);
                    let inner = &body[..end];
                    spans.extend(render_inline(
                        inner,
                        Some(base.add_modifier(Modifier::ITALIC)),
                    ));
                    i += 1 + end + 1;
                    continue;
                }
            }
        }

        // [label](url)
        if rest.starts_with('[') {
            if let Some((label, url, consumed)) = parse_link(rest) {
                push_plain(&mut spans, &mut buf, base);
                spans.push(Span::styled(
                    label.to_string(),
                    base.fg(Color::Blue).add_modifier(Modifier::UNDERLINED),
                ));
                spans.push(Span::styled(
                    format!(" ({url})"),
                    base.fg(Color::DarkGray),
                ));
                i += consumed;
                continue;
            }
        }

        // default: copy this char into the plain buffer.
        let ch = rest.chars().next().unwrap();
        buf.push(ch);
        i += ch.len_utf8();
    }
    push_plain(&mut spans, &mut buf, base);
    spans
}

fn parse_link(s: &str) -> Option<(&str, &str, usize)> {
    debug_assert!(s.starts_with('['));
    let end_label = s.find(']')?;
    let after = &s[end_label + 1..];
    if !after.starts_with('(') {
        return None;
    }
    let end_url = after.find(')')?;
    let label = &s[1..end_label];
    let url = &after[1..end_url];
    // total bytes consumed: '[' label ']' '(' url ')'
    let consumed = end_label + 1 + end_url + 1;
    Some((label, url, consumed))
}

/// True when the user has opted out of M5 rendering.
/// Honoured at the call site (render_transcript) so the change is
/// observable without touching the action loop.
pub fn minimal_mode_enabled() -> bool {
    std::env::var_os("CC_TUI_MINIMAL").is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, widgets::Paragraph, Terminal};

    fn lines_to_string(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn render_in_80x24(lines: &[Line<'static>]) -> String {
        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            let para =
                Paragraph::new(lines.to_vec()).wrap(ratatui::widgets::Wrap { trim: false });
            f.render_widget(para, f.area());
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let mut s = String::new();
        let area = buf.area;
        for y in 0..area.height {
            for x in 0..area.width {
                s.push_str(buf[(x, y)].symbol());
            }
            s.push('\n');
        }
        s
    }

    #[test]
    fn plain_text_round_trips() {
        let out = render_markdown("hello world");
        assert_eq!(lines_to_string(&out), "hello world");
    }

    #[test]
    fn bold_italic_code_split_into_spans() {
        let out = render_markdown("a **b** c *d* e `f` g");
        // One Line, five spans wrapping: "a ", "b", " c ", "d", " e ", "f", " g"
        assert_eq!(out.len(), 1);
        let plain = lines_to_string(&out);
        assert_eq!(plain, "a b c d e f g");

        // Style check: the bold span should carry Modifier::BOLD.
        let bold = &out[0]
            .spans
            .iter()
            .find(|s| s.content == "b")
            .expect("bold span missing")
            .style;
        assert!(
            bold.add_modifier.contains(Modifier::BOLD),
            "bold span must have BOLD modifier"
        );
        let italic = &out[0]
            .spans
            .iter()
            .find(|s| s.content == "d")
            .expect("italic span missing")
            .style;
        assert!(
            italic.add_modifier.contains(Modifier::ITALIC),
            "italic span must have ITALIC modifier"
        );
    }

    #[test]
    fn fenced_code_block_is_bordered() {
        let input = "before\n```rust\nlet x = 1;\n```\nafter";
        let out = render_markdown(input);
        let rendered = render_in_80x24(&out);
        assert!(rendered.contains("rust"), "language label missing");
        assert!(rendered.contains("let x = 1;"), "code body missing");
        assert!(rendered.contains("└─"), "fence close missing");
    }

    #[test]
    fn unterminated_fence_shows_streaming_hint() {
        let input = "```\npartial\n";
        let out = render_markdown(input);
        let rendered = render_in_80x24(&out);
        assert!(
            rendered.contains("streaming"),
            "unterminated fence must hint at streaming"
        );
    }

    #[test]
    fn headings_have_distinct_prefixes() {
        let out = render_markdown("# h1\n## h2\n### h3");
        let plain = lines_to_string(&out);
        // Each prefix glyph is distinct so a screen reader / visual
        // test can tell them apart.
        assert!(plain.contains("█ h1"));
        assert!(plain.contains("▌ h2"));
        assert!(plain.contains("▎ h3"));
    }

    #[test]
    fn bullet_list_renders_dot() {
        let out = render_markdown("- first\n- second");
        let plain = lines_to_string(&out);
        assert!(plain.contains("• first"));
        assert!(plain.contains("• second"));
    }

    #[test]
    fn numbered_list_renders_number() {
        let out = render_markdown("1. first\n2. second");
        let plain = lines_to_string(&out);
        assert!(plain.contains("1. first"));
        assert!(plain.contains("2. second"));
    }

    #[test]
    fn link_label_carries_underline() {
        let out = render_markdown("see [docs](https://x.y)");
        // There should be a span with content "docs" carrying UNDERLINED.
        let label = out[0]
            .spans
            .iter()
            .find(|s| s.content == "docs")
            .expect("link label span missing");
        assert!(
            label.style.add_modifier.contains(Modifier::UNDERLINED),
            "link label must be underlined"
        );
    }

    #[test]
    fn ac_v1_round_trip_all_six_elements() {
        // AC-V1: bold, italic, inline code, fenced code, bullet list, `###`
        // heading must all render visually distinct on the same screen.
        let input = "\
### heading three
- first bullet
- with **bold**, *italic*, and `code`

```rust
fn main() {}
```
";
        let out = render_markdown(input);
        let rendered = render_in_80x24(&out);
        assert!(rendered.contains("▎ heading three"), "heading missing");
        assert!(rendered.contains("• first bullet"), "bullet missing");
        assert!(rendered.contains("rust"), "fenced code language missing");
        assert!(rendered.contains("fn main() {}"), "code body missing");
        // Style-level assertions for bold / italic / inline code.
        let flat: Vec<&Span<'_>> = out.iter().flat_map(|l| l.spans.iter()).collect();
        let has_bold = flat
            .iter()
            .any(|s| s.content == "bold" && s.style.add_modifier.contains(Modifier::BOLD));
        let has_italic = flat
            .iter()
            .any(|s| s.content == "italic" && s.style.add_modifier.contains(Modifier::ITALIC));
        let has_code = flat.iter().any(|s| s.content == "code");
        assert!(has_bold, "bold span missing");
        assert!(has_italic, "italic span missing");
        assert!(has_code, "inline code span missing");
    }

    #[test]
    fn horizontal_rule_rendered() {
        let out = render_markdown("above\n---\nbelow");
        let plain = lines_to_string(&out);
        assert!(plain.contains("─"), "hr missing");
    }

    #[test]
    fn blockquote_has_bar_prefix() {
        let out = render_markdown("> quoted");
        let plain = lines_to_string(&out);
        assert!(plain.contains("▏ quoted"));
    }

    #[test]
    fn fuzz_arbitrary_ascii_never_panics() {
        // Feed 8 KiB of pseudo-random ASCII and assert no panic.
        let mut s = String::with_capacity(8 * 1024);
        let mut seed = 0x9e3779b9u32;
        for _ in 0..8192 {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let c = (32u8 + (seed & 0x3f) as u8) as char;
            s.push(c);
            if seed & 0x0f == 0 {
                s.push('\n');
            }
        }
        let _ = render_markdown(&s);
    }

    #[test]
    fn minimal_mode_toggle_reads_env() {
        // SAFETY: set_var is safe in single-threaded test setup.
        std::env::set_var("CC_TUI_MINIMAL", "1");
        assert!(minimal_mode_enabled());
        std::env::remove_var("CC_TUI_MINIMAL");
        assert!(!minimal_mode_enabled());
    }
}
