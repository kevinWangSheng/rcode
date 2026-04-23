## ADDED Requirements

### Requirement: Input Caret Is Always Visible

The TUI input box SHALL keep the composing caret inside the visible
rendered region, regardless of buffer length. When the buffer is
wider than the inner area, the box SHALL maintain a horizontal view
offset so that the caret's display column is in
`[offset + 1, offset + inner_width - 2]` (one-cell margin on each
side). When the buffer fits, no scrolling is performed.

The right-edge caret clamp in `render_input` (line ~570) MAY remain
as a defensive no-op, but MUST NOT be the primary means of keeping
the caret inside the widget.

#### Scenario: Typing past the right edge
- **GIVEN** the user has typed enough characters to fill the inner
  area
- **WHEN** they type one more character
- **THEN** the view scrolls left by at least one cell AND the newly
  typed character is visible AND the caret is visible.

#### Scenario: Home returns the view to the start
- **GIVEN** the buffer is scrolled to show the tail
- **WHEN** the user presses Home
- **THEN** the view offset resets to 0 AND the first character of
  the buffer is the first content cell AND the caret is at col 3.

#### Scenario: End scrolls to the tail
- **GIVEN** the buffer is 200 chars and the view is at offset 0
- **WHEN** the user presses End
- **THEN** the last character of the buffer is visible at or near
  the right edge AND the caret is on the right side of the
  visible area (not clamped past it).

#### Scenario: Left at each press moves the caret visibly
- **GIVEN** a 100-char buffer at End on an 80-col terminal
- **WHEN** the user presses Left 26 times
- **THEN** on every press the rendered caret column decreases by
  at least one cell (no "invisible" presses).

#### Scenario: CJK on the boundary
- **GIVEN** a buffer composed of full-width CJK ideographs
- **WHEN** the view reflows
- **THEN** `input_view_offset` is on a cell boundary such that no
  full-width glyph is rendered as half a cell; the caret sits
  between full-width glyphs.
