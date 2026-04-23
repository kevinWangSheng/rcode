# Bug Report — TUI input box has no horizontal scroll

## Summary

Typing past the visible width of the input box silently drops the
rendered characters and clamps the caret to the right edge. Cursor
keys become "invisible": the logical caret moves but nothing on
screen changes for many keypresses.

## Environment

- **Repo commit:** `f9fd015aaa86e64c846bdf99d4ad402789be8348`
  (branch `phase3/implementation`)
- **Binary:** `target/debug/cc-tui-demo` (cc-tui 0.1.0)
- **Date:** 2026-04-22
- **Terminal:** 80 cols × 24 rows, via `npcterm` MCP PTY host
  (`terminal_create size=80x24`)
- **OS:** macOS (Darwin 25.1.0)
- **Build:** `cargo build -p cc-tui --bin cc-tui-demo` (default
  features)

## Severity / Priority

- **Severity:** Major. Composing any message longer than one line
  in an 80-col terminal is effectively broken — the user types
  invisibly.
- **Priority:** High. Hit by every user of the TUI on a standard
  terminal size.

## Preconditions

- `cc-tui-demo` launched with `CC_TUI_DEMO_SCRIPT=/tmp/tui_script_1.json`
  where the script is `[{"sleep_ms":300}]` (script that does
  nothing; we just want an idle TUI).
- TUI is at the welcome screen, `AppMode::Input`, empty buffer.

## Reproduction Steps

1. Launch `cc-tui-demo` in an 80×24 terminal with the no-op script.
2. Type 100 ASCII characters (e.g. `"0123456789"` repeated 10 times)
   via a single batched input.
3. Observe the input-box contents and `cursor_pos`.
4. Press `Home`. Observe.
5. Press `End`. Observe.
6. From End, press `Left` 25 times. After each press observe
   `cursor_pos`.
7. Press `Left` one more time (26th).

## Expected Result

- Step 3: every typed character visible somewhere (viewport scrolls
  left as needed); caret visible at the right edge of content.
- Step 4: caret visible at col 3 (start of content); viewport
  resets to show buffer start.
- Step 5: caret visible at the right edge with the TAIL of the
  buffer shown inside the box.
- Step 6: every `Left` press moves the caret visibly by one cell
  (or scrolls the viewport).
- Step 7: same.

## Actual Result

- Step 3: only the first 76 chars render inside the box; chars
  76-99 are absent from the screen; caret clamps at col 78.
- Step 4: caret moves to col 3 (logically correct), but the same
  first-76 chars are still shown — `/` at col 3 reveals the
  beginning of buffer (this arm happens to look OK).
- Step 5: caret back at col 78; the TAIL chars are still NOT shown
  — the viewport never scrolls; the caret sits on the last visible
  char `5` which is position 75, NOT position 100.
- Step 6: caret does NOT move for 25 consecutive presses; stays at
  col 78.
- Step 7: on the 26th press, caret finally moves to col 77.

## Evidence (verbatim PTY capture)

`/tmp/tui_bug_evidence/bug1_after_type_100chars.txt`:

```
cursor_pos: (78, 20)
logical buffer length: 100 chars

19 ┌──────────────────────────────────────────────────────────────────────────────┐
20 │> 0123456789012345678901234567890123456789012345678901234567890123456789012345│
21 └──────────────────────────────────────────────────────────────────────────────┘

Visible: 76 cells. Trailing 24 chars "6789012345678901234567890123" invisible.
```

`/tmp/tui_bug_evidence/bug1_left_invisible.txt`:

```
  N=0   cursor_pos=(78,20)  # End — clamped at right edge
  N=1   cursor_pos=(78,20)  # still clamped
  ...
  N=25  cursor_pos=(78,20)  # STILL clamped — 25 consecutive invisible keypresses
  N=26  cursor_pos=(77,20)  # finally moves
```

## Root Cause

`crates/cc-tui/src/render.rs::render_input` (lines 505-574) renders
the whole buffer as a single-line `Paragraph`:

```rust
// render.rs:528-534
spans.push(Span::styled(
    app.input.clone(),
    Style::default().fg(theme.text),
));
let para = Paragraph::new(Line::from(spans)).block(...);
```

Ratatui's `Paragraph` truncates overlong content at the right edge of
the widget area. No `App` field tracks a horizontal scroll offset, so
overflow text is simply discarded on draw.

The caret position is then clamped:

```rust
// render.rs:561-570
let cursor_offset = app.input_cursor.min(app.input.len());
let before_caret = &app.input[..cursor_offset];
let prefix_width = UnicodeWidthStr::width(before_caret) as u16;
let gutter_cells: u16 = 2;
let cursor_x = area.x
    .saturating_add(1)
    .saturating_add(gutter_cells)
    .saturating_add(prefix_width)
    .min(area.x.saturating_add(area.width).saturating_sub(2)); // 78 for width=80
```

Once `prefix_width + 3 >= 78` the `min()` pins the caret to col 78
forever; visible caret movement only resumes when logical caret
returns to position < 75. For a 100-char buffer that is 26 `Left`
presses.

## Affected Scope / Blast Radius

- Every terminal narrower than the user's average composing line
  exhibits the bug. On the common 80-col terminal, that is any
  message ≥ 76 display cells (≈ 15 English words).
- CJK-heavy input halves the threshold because each CJK glyph costs
  2 cells.
- Paste of a long command, URL, or file path triggers the bug
  instantly.
- Does NOT affect: streaming transcript rendering, tool blocks,
  slash-command palette filter (those are separate render paths).

## Fix Direction

1. Add `input_view_offset: usize` (display cells) to `App`.
2. Maintain the offset after every edit (`input_insert_char`,
   `input_insert_str`, `input_backspace`, `input_delete`,
   `clear_input`) and every cursor move (`input_cursor_left`,
   `input_cursor_right`, `Home`, `End`).
3. Invariant: caret_display_col ∈ [offset + 1,
   offset + inner_width − 2] (one-cell margin each side).
4. `render_input` slices from `input_view_offset` and advances by
   `UnicodeWidthStr::width` so CJK/emoji land on cell boundaries.
5. Keep the existing right-edge clamp as a safety net; it MUST not
   normally fire once offset tracks the caret.
6. Buffers shorter than `inner_width`: offset stays 0 (no visible
   change today).

## Regression Risk

- **LOW.** Short-buffer behavior is unchanged (offset always 0 when
  buffer fits).
- Headless render tests for short buffers (most of the existing
  suite) will still pass.
- New invariant: `reflow_input_viewport` must be called after every
  edit/move. A new headless test per acceptance-criterion (see
  `tasks.md`) will catch drop-throughs.

## Out of Scope

- Multi-line composing (Shift+Enter style newlines). The current
  `Paragraph::new(Line::from(...))` hard-codes a single logical
  line; a multi-line input box is a separate design.
- Bidirectional text / RTL caret movement.

## Why (openspec field)

See the Summary above.

## What Changes (openspec field)

See Fix Direction above. Concretely:

- **Affected code:** `cc-tui/src/app.rs` (add `input_view_offset`
  + helper), `cc-tui/src/action.rs` (recompute on edit / cursor
  move), `cc-tui/src/render.rs::render_input`.

## Capabilities

### Modified Capabilities
- `tui-input-box`: the input area MUST scroll horizontally so the
  caret is always visible and every character the user types is
  reflected somewhere on screen, even when the buffer is wider than
  the inner area.

## Impact

- **Risk:** LOW (see Regression Risk above).
- **Tests:** add headless cases for edit-at-end, Home-then-End,
  CJK caret placement near the right edge, and the 26-Left-presses
  regression (see `tasks.md`).
