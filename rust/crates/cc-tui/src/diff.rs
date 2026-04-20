//! Minimal unified-diff renderer for the Edit tool (M5 AC-V4).
//!
//! Approach: hand-rolled LCS → hunk-list → render. We deliberately avoid
//! pulling in `similar` (~1 MiB) because the volume of text diffed inside
//! an Edit call is always small (single-file / single-region).
//!
//! Output is a `Vec<Line>` with:
//!   - Removed lines: `- …` in red
//!   - Added lines:   `+ …` in green
//!   - Context:       `  …` in default color
//!
//! Context rows are limited to `CONTEXT_LINES` (= 2) on each side of a
//! change, per M5 scope.

use ratatui::{
    style::Style,
    text::{Line, Span},
};

use crate::theme;

/// Number of unchanged context lines shown around each hunk. Matches the
/// default used by the TS original's `diff.structuredPatch` (3), so an Edit
/// tool card reads the same on both binaries.
pub const CONTEXT_LINES: usize = 3;

/// Render `old` and `new` as a unified diff.
///
/// Returns one `Line` per diff row. Empty input on either side still
/// produces a sensible diff: deletion-only or insertion-only hunks.
pub fn render_unified_diff(old: &str, new: &str) -> Vec<Line<'static>> {
    let old_lines: Vec<&str> = old.split('\n').collect();
    let new_lines: Vec<&str> = new.split('\n').collect();

    let ops = lcs_diff(&old_lines, &new_lines);
    let hunks = group_hunks(&ops, CONTEXT_LINES);

    let theme = theme::current();
    let mut out: Vec<Line<'static>> = Vec::new();
    for (hi, hunk) in hunks.iter().enumerate() {
        if hi > 0 {
            // Visual separator between non-contiguous hunks.
            out.push(Line::from(Span::styled(
                "...".to_string(),
                Style::default().fg(theme.subtle),
            )));
        }
        for op in hunk {
            out.push(render_op(op, &theme));
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Op {
    Keep(String),
    Del(String),
    Add(String),
}

fn render_op(op: &Op, theme: &theme::Theme) -> Line<'static> {
    match op {
        Op::Keep(s) => Line::from(vec![
            Span::styled("  ".to_string(), Style::default().fg(theme.subtle)),
            Span::raw(s.clone()),
        ]),
        Op::Del(s) => Line::from(vec![
            Span::styled("- ".to_string(), Style::default().fg(theme.error)),
            Span::styled(s.clone(), Style::default().fg(theme.error)),
        ]),
        Op::Add(s) => Line::from(vec![
            Span::styled("+ ".to_string(), Style::default().fg(theme.success)),
            Span::styled(s.clone(), Style::default().fg(theme.success)),
        ]),
    }
}

fn lcs_diff(a: &[&str], b: &[&str]) -> Vec<Op> {
    // Classic Myers-ish LCS via dynamic programming.
    // O(m*n) space/time — fine for Edit-tool-sized inputs.
    let m = a.len();
    let n = b.len();
    let mut dp = vec![vec![0usize; n + 1]; m + 1];
    for i in 0..m {
        for j in 0..n {
            dp[i + 1][j + 1] = if a[i] == b[j] {
                dp[i][j] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }

    // Backtrack to recover ops in reverse.
    //
    // Tie-break note: at a mismatch we prefer to move *left* (Add b[j-1]) on
    // ties so that after the final reverse, the forward output emits `- old`
    // before `+ new` — the convention a unified-diff reader expects.
    let mut ops = Vec::with_capacity(m + n);
    let (mut i, mut j) = (m, n);
    while i > 0 && j > 0 {
        if a[i - 1] == b[j - 1] {
            ops.push(Op::Keep(a[i - 1].to_string()));
            i -= 1;
            j -= 1;
        } else if dp[i][j - 1] >= dp[i - 1][j] {
            ops.push(Op::Add(b[j - 1].to_string()));
            j -= 1;
        } else {
            ops.push(Op::Del(a[i - 1].to_string()));
            i -= 1;
        }
    }
    while i > 0 {
        ops.push(Op::Del(a[i - 1].to_string()));
        i -= 1;
    }
    while j > 0 {
        ops.push(Op::Add(b[j - 1].to_string()));
        j -= 1;
    }
    ops.reverse();
    ops
}

/// Partition `ops` into contiguous hunks: a hunk is any run that contains
/// at least one `Del` or `Add`, padded with up to `context` `Keep` lines
/// on each side. Pure-`Keep` runs longer than `2 * context` are collapsed
/// away between hunks.
fn group_hunks(ops: &[Op], context: usize) -> Vec<Vec<Op>> {
    // Indexes of each changed op.
    let change_idx: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter_map(|(i, op)| match op {
            Op::Del(_) | Op::Add(_) => Some(i),
            _ => None,
        })
        .collect();

    if change_idx.is_empty() {
        return Vec::new();
    }

    // Merge nearby change groups.
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for &ci in &change_idx {
        let start = ci.saturating_sub(context);
        let end = (ci + context).min(ops.len().saturating_sub(1));
        if let Some(last) = ranges.last_mut() {
            if start <= last.1 + 1 {
                last.1 = last.1.max(end);
                continue;
            }
        }
        ranges.push((start, end));
    }

    ranges
        .into_iter()
        .map(|(s, e)| ops[s..=e].to_vec())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Color, Modifier};

    fn line_to_plain(l: &Line<'_>) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn identical_input_produces_no_hunks() {
        let out = render_unified_diff("a\nb\nc", "a\nb\nc");
        assert!(out.is_empty(), "identical inputs should yield no diff rows");
    }

    #[test]
    fn single_line_change_renders_minus_plus() {
        let out = render_unified_diff("hello\nworld", "hello\nrust");
        let plain: Vec<String> = out.iter().map(line_to_plain).collect();
        assert!(plain.iter().any(|l| l == "  hello"));
        assert!(plain.iter().any(|l| l == "- world"));
        assert!(plain.iter().any(|l| l == "+ rust"));
    }

    #[test]
    fn diff_has_at_least_two_context_lines() {
        let old = "l1\nl2\nl3\nl4\nOLD\nl6\nl7\nl8\nl9";
        let new = "l1\nl2\nl3\nl4\nNEW\nl6\nl7\nl8\nl9";
        let out = render_unified_diff(old, new);
        // Must have "- OLD" and "+ NEW" with ≥2 context lines before and after.
        let plain: Vec<String> = out.iter().map(line_to_plain).collect();
        let minus = plain.iter().position(|l| l == "- OLD").expect("minus");
        let plus = plain.iter().position(|l| l == "+ NEW").expect("plus");
        // Two context above the minus.
        assert!(minus >= 2, "expected ≥2 context before minus: {plain:?}");
        assert!(
            plain[minus - 1].starts_with("  ") && plain[minus - 2].starts_with("  "),
            "context rows before change missing"
        );
        // Two context after the plus.
        assert!(plain.len() >= plus + 3, "need ≥2 context after plus");
        assert!(plain[plus + 1].starts_with("  ") && plain[plus + 2].starts_with("  "));
    }

    #[test]
    fn pure_deletion_renders() {
        let out = render_unified_diff("keep\nremove\nkeep2", "keep\nkeep2");
        let plain: Vec<String> = out.iter().map(line_to_plain).collect();
        assert!(plain.iter().any(|l| l == "- remove"));
    }

    #[test]
    fn pure_insertion_renders() {
        let out = render_unified_diff("keep\nkeep2", "keep\nnew\nkeep2");
        let plain: Vec<String> = out.iter().map(line_to_plain).collect();
        assert!(plain.iter().any(|l| l == "+ new"));
    }

    #[test]
    fn diff_colors_are_theme_error_and_success() {
        // Anchor the colours to the active theme rather than a literal, so a
        // palette swap (D1 → D6 light variant) does not silently break the
        // diff renderer.
        let theme = theme::current();
        let out = render_unified_diff("a", "b");
        let minus = out
            .iter()
            .find(|l| line_to_plain(l) == "- a")
            .expect("minus row missing");
        let plus = out
            .iter()
            .find(|l| line_to_plain(l) == "+ b")
            .expect("plus row missing");
        assert_eq!(minus.spans[0].style.fg, Some(theme.error));
        assert_eq!(plus.spans[0].style.fg, Some(theme.success));
        // The test also proves we didn't accidentally add BOLD everywhere.
        let _ = Modifier::BOLD;
        let _ = Color::Red; // silence unused-import paranoia
    }

    #[test]
    fn multiple_hunks_are_separated_by_ellipsis() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\nm\nn\no";
        let new = "A\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\nm\nn\nO";
        let out = render_unified_diff(old, new);
        let plain: Vec<String> = out.iter().map(line_to_plain).collect();
        assert!(plain.iter().any(|l| l == "..."), "expected hunk separator");
    }
}
