# Bug Report — /help output wraps with zero-indent continuation, breaking vertical alignment

## Summary

On an 80-column terminal, the `/help` output contains at least one
entry (`/reload-keybindings`) whose rendered line exceeds the
viewport width. Ratatui's `Paragraph::wrap(Wrap { trim: false })`
wraps the overflow to a new row but starts the continuation at
column 0 — zero indent — while every other `/help` entry begins
at column 6. The reader's eye loses the description-continuation
relationship.

## Environment

- **Repo commit:** `f9fd015aaa86e64c846bdf99d4ad402789be8348`
  (branch `phase3/implementation`)
- **Binary:** `target/debug/cc-tui-demo` (cc-tui 0.1.0)
- **Date:** 2026-04-22
- **Terminal:** 80 cols × 24 rows, via `npcterm` MCP

## Severity / Priority

- **Severity:** Minor (cosmetic). No loss of information, no
  interaction breakage.
- **Priority:** Low-to-Medium. Every user who runs `/help` at 80
  cols sees it.

## Preconditions

- `cc-tui-demo` launched, at the welcome screen.

## Reproduction Steps

1. Launch `cc-tui-demo` in an 80×24 terminal.
2. Type `/help` and press Enter.
3. Press Esc to dismiss the residual palette popup (see
   `fix-tui-palette-stale-after-submit`).
4. Observe the bottom of the rendered `/help` output.

## Expected Result

Every rendered row begins at column 6 or later (matching the
visual column of `/name` in short-name entries such as `/version`,
`/cost`), so the block reads as a coherent list.

## Actual Result

The `/reload-keybindings` entry wraps across two rows. The first
row starts at column 6 (consistent), the continuation tail `the
TUI` starts at **column 0** — visually detached from its parent
entry.

## Evidence (verbatim PTY capture)

`/tmp/tui_bug_evidence/bug3_help_wrap.txt`:

```
   00000000001111111111222222222233333333334444444444555555555566666666667777777777
   01234567890123456789012345678901234567890123456789012345678901234567890123456789
00      /version    show version
01      /model      show or switch the active model (`/model <name>`)
...
15      /context    show context window usage (tokens remaining)
16      /reload-keybindings re-read ~/.claude/keybindings.json without restarting
17 the TUI
```

- Row 16 content column 6 → `/reload-keybindings …`.
- Row 17 content column 0 → `the TUI` — no indent.

## Root Cause

Two-layer cause:

1. `CommandRegistry::help_text` in
   `crates/cc-tui/src/commands.rs` line 313 formats every built-in
   with a fixed 10-char pad:

   ```rust
   out.push_str(&format!("  /{:<10} {}\n", b.name(), b.description()));
   ```

   For a 10-or-shorter name, the rendered single line fits within
   80 cols (visible `/` at col 6; description column at col 18).
   For `reload-keybindings` (18 chars), the pad does nothing and
   the line exceeds ~80 cells.

2. `push_system_notice` in `crates/cc-tui/src/render.rs` line 365
   inserts a 3-space prefix on every non-first line, yielding a
   per-line structure of roughly "{3-space notice indent}{2-space
   format literal}{/name}{pad}{1-space}{description}". That full
   line is then handed to ratatui's `Paragraph::new(lines)
   .wrap(Wrap { trim: false })` at line 170. Ratatui wraps at
   viewport width with NO hanging-indent option — the wrapped
   continuation starts at column 0 of the widget area.

Either layer alone can be fixed; fixing (1) is cheaper and does
not invent a hanging-indent mechanism that ratatui does not
provide.

## Affected Scope / Blast Radius

- Only `/help` output — the single system notice that is wider
  than the viewport.
- On terminals wider than the widest `/help` entry (roughly ≥ 90
  cols) the wrap never fires; the bug is 80-col-specific. On
  narrower terminals (60-cols, 70-cols — less common but real),
  MORE entries wrap, so MORE zero-indent continuation lines
  appear.
- Similar risk exists for long skill / MCP names registered by
  users (same format string at line 319, 466) — same fix family
  applies.

## Fix Direction

Introduce a helper in `commands.rs`:

```rust
fn format_entry(name: &str, desc: &str) -> String {
    if name.len() <= 10 {
        format!("  /{:<10} {}\n", name, desc)
    } else {
        // Emit name on its own line; description on the next line
        // padded so it aligns with the description column of the
        // short-name form (col 14 in the logical string).
        format!("  /{}\n              {}\n", name, desc)
    }
}
```

- Single-line form for names ≤ 10 chars — byte-identical to today.
- Two-line form for long names — the description column
  (14 spaces of indent in the source string) is what short-name
  entries use after `"  /"` + 10-char pad + `" "`. Rendered
  through `push_system_notice`'s 3-space prefix, the description
  lands on the same on-screen column as short-name entries.
- Reuse the helper at the three `help_text`-style call sites
  (built-ins, skills, MCP skill listing).

Also add a small regression test that asserts no rendered
non-blank line of the `/help` notice starts at column 0 on an
80-col backend.

## Regression Risk

- **LOW.** Names ≤ 10 chars (every current built-in except
  `reload-keybindings` and `permissions`) use the existing
  formatter and produce byte-identical output.
- `permissions` is 11 chars — it also passes the `name.len() <=
  10` threshold as FALSE. Manually verify its two-line output
  looks OK (the current output already deviates from the `:<10`
  alignment for `permissions` — this fix improves it).
- Does not change any non-help-text formatter.

## Out of Scope

- Introducing a generic hanging-indent wrap in
  `push_system_notice` — a bigger API surface for a single
  observed offender.
- Reflowing at terminal width dynamically — out of scope, the
  two-line form is correct regardless of width.

## Why (openspec field)

See Summary.

## What Changes (openspec field)

- `cc-tui/src/commands.rs::help_text` (+ the skills-list loop at
  line 319 + `render_skills` at line 466) switches to the
  `format_entry` helper.
- No render-layer changes.

## Capabilities

### Modified Capabilities
- `tui-help-output`: `/help` MUST NOT produce a rendered
  continuation row starting at column 0 on any terminal width ≥
  40 cells.

## Impact

- **Affected code:** `cc-tui/src/commands.rs`.
- **Risk:** LOW (see Regression Risk).
