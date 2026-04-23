## ADDED Requirements

### Requirement: Palette Closes After Any Slash-Command Submit

When the user submits from `AppMode::CommandPalette`, the TUI SHALL,
as part of the same update cycle, clear `palette_matches`,
`palette_selected`, `palette_original`, and set
`AppMode::Input` BEFORE any outcome-specific branch runs. Only a
`SubmitUserMessage` outcome MAY subsequently override mode to
`AppMode::Streaming` via its own `start_stream` call.

The help footer (`render_help_footer`) derives its text from
`app.mode`, so this single state update is sufficient to fix all
observed footer/mode mismatches — no footer-specific code changes.

#### Scenario: Info command dismisses the palette
- **GIVEN** the palette is open with "/help" typed
- **WHEN** the user presses Enter
- **THEN** the "ⓘ ..." system notice appears in the transcript
  AND the palette popup is not rendered
  AND `app.mode == AppMode::Input`.

#### Scenario: Clear command dismisses the palette
- **GIVEN** the palette is open with "/clear" typed
- **WHEN** the user presses Enter
- **THEN** the transcript is cleared
  AND "ⓘ  Transcript cleared." is visible
  AND the palette popup is not rendered
  AND `app.mode == AppMode::Input`.

#### Scenario: User message still enters streaming
- **GIVEN** the input contains a non-slash message, e.g. "hi"
- **WHEN** the user presses Enter
- **THEN** the user message is pushed into the transcript
  AND `app.mode == AppMode::Streaming`
  (the palette-close-on-submit default is overridden by
  `start_stream`).

#### Scenario: Empty submit is a no-op
- **GIVEN** the palette is open with only "/" in the buffer
- **WHEN** the user presses Enter (text is empty after parse)
- **THEN** the buffer, mode, and palette state are unchanged.
