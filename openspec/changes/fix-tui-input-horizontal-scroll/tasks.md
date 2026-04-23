## 1. App State

- [x] 1.1 Add `input_view_offset: usize` (display cells) to `App`
      in `crates/cc-tui/src/app.rs`.
- [x] 1.2 Reset to 0 in `App::clear_input`.
- [x] 1.3 Ensure `App::new`/`Default` initialise it to 0.

## 2. Offset Maintenance

- [x] 2.1 Introduce `App::reflow_input_viewport(inner_width: u16)`
      in `app.rs`. Invariant after call:
      `caret_display_col ∈ [offset + 1, offset + inner_width - 2]`
      (one-cell margin on each side). Uses `UnicodeWidthStr::width`
      so offset lands on cell boundaries (never splits a CJK
      glyph).
- [x] 2.2 Call `reflow_input_viewport` from the
      `AppAction::InsertChar`, `AppAction::Backspace`,
      `AppAction::DeleteChar`, `AppAction::CursorMove` arms in
      `action.rs::update` — and from the Home/End arms. Pass the
      inner width computed the same way `render_input` does
      (`area.width - 2 borders - 2 gutter`).
- [x] 2.3 Document the invariant in a doc comment on
      `reflow_input_viewport`.

## 3. Rendering

- [x] 3.1 In `render.rs::render_input`, slice the buffer starting
      at `input_view_offset` display cells (NOT bytes), using
      `UnicodeWidthStr::width` to advance, and stop at
      `inner_width - gutter_cells`.
- [x] 3.2 Caret X becomes `area.x + 1 + gutter_cells +
      (caret_display_col - input_view_offset)`. Keep the existing
      right-edge `min()` clamp as a safety net.
- [x] 3.3 No change when buffer fits inside `inner_width`
      (offset stays 0, same bytes rendered as today).

## 4. Tests (headless, `crates/cc-tui/tests/headless.rs`)

- [x] 4.1 `typing_past_width_keeps_caret_visible`: type
      200 `'x'` chars on an 80-col backend; assert caret column is
      strictly less than `inner_right_edge` AND the last char
      typed appears somewhere in the rendered row.
- [x] 4.2 `home_resets_viewport`: type 200 chars, press Home,
      assert caret at col 3 AND first char of buffer visible.
- [x] 4.3 `end_scrolls_to_tail`: type 200 chars, press Home, press
      End, assert caret at col 77 or 78 AND the LAST char of the
      buffer is visible in the rendered row.
- [x] 4.4 `left_moves_caret_every_press`: from End with a 100-char
      buffer, call Left 26 times; assert caret x decreases
      monotonically by ≥ 1 cell on every press. Regression guard
      for the 25-invisible-presses symptom.
- [x] 4.5 `cjk_caret_on_cell_boundary`: buffer "你好" × 50, End;
      assert caret x is on a cell boundary (not between the two
      halves of a full-width glyph) AND the rightmost visible
      glyph is fully rendered.

## 5. Manual Verification

- [x] 5.1 `npcterm` PTY: launch `cc-tui-demo` at 80×24, type the
      100-char ASCII string from the bug repro, walk Home → End →
      Left × 30, and eyeball that the caret tracks content on
      every press. Attach the new capture under
      `/tmp/tui_bug_evidence/bug1_after_fix.txt`.

## 6. Sign-off

- [x] 6.1 `cargo test -p cc-tui` green.
- [x] 6.2 `cargo clippy -p cc-tui --all-targets` clean.
- [x] 6.3 Spot-check `headless.rs` + `pty.rs` for drift.
