## ADDED Requirements

### Requirement: Help Output Preserves Indent on Wrap

The `/help` system notice SHALL render every built-in and skill
entry such that, on any terminal ≥ 40 cols wide, no rendered line
begins at column 0. Commands whose name exceeds 10 characters
MUST use a two-line form where the description sits on its own
line with leading whitespace that aligns it with the description
column used by shorter-name entries.

#### Scenario: Short command name renders as one line
- **GIVEN** a built-in named `help` with description
  "show this help"
- **WHEN** `help_text()` is formatted
- **THEN** the entry is the single line
  `"  /help       show this help\n"` (byte-identical to pre-fix
  output).

#### Scenario: Long command name uses two-line form
- **GIVEN** a built-in named `reload-keybindings` with a long
  description
- **WHEN** `help_text()` is formatted
- **THEN** the entry spans two source lines: first line
  `"  /reload-keybindings\n"`; second line begins with 14 spaces
  of leading whitespace so that after the system-notice 3-space
  prefix, the description renders in the same visual column as
  the short-name descriptions.

#### Scenario: No zero-column continuations on any width
- **GIVEN** `help_text()` rendered into an 80-col `TestBackend`
- **WHEN** the output is inspected
- **THEN** every non-blank row starts with at least two space
  characters.

#### Scenario: Threshold is at 10 characters
- **GIVEN** a name of length 10 and a name of length 11
- **WHEN** `format_entry(name, desc)` is called
- **THEN** the length-10 name uses the one-line form; the
  length-11 name uses the two-line form.
