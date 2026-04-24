# TUI design critique — 2026-04-24

Captured after running `target/debug/claude` through npcterm at
160×40 and walking through: launch / empty-session welcome /
short reply / long markdown reply (Rust ownership) / slash
palette / permission modal + Bash tool with stderr error /
multi-turn / exit. Phase3/implementation HEAD `9b85e47`.

This is a **backlog, not a work plan**. File openspec changes per
fix when picked up.

## P0 — visibly broken

### 1. Permission modal leaves ghost borders + stale help footer after dismissal

**Repro**: ask the model to run a Bash tool, accept or deny the
`y/a/n` prompt. After the modal closes, fragments of its border
(`│`, `└─`) remain painted behind the streaming content for the
rest of the turn; the help footer stays locked at `y allow · a
always · n deny` instead of returning to the mode-appropriate
hint.

**Root cause**: Ratatui's buffer-diff doesn't invalidate cells
around an overlay that just unmounted. The modal paints a
`Clear` rect when it renders, but there's no equivalent cleanup
pass when the app transitions out of `PermissionPrompt` mode —
the cells that held the modal's border aren't marked dirty, so
the streaming text that re-renders underneath writes *around*
them, leaving the border chars intact. Help-footer is the same
shape: the `render_help_footer` match on `AppMode` still reads
the stale mode somewhere, or the footer wasn't invalidated.

**Fix sketch**: on `AppMode` transition out of `PermissionPrompt`,
issue a `frame.render_widget(Clear, permission_modal_rect)`
during the NEXT draw (stash the rect on `App` when the modal
opens). Same treatment for the palette when it closes. Add a
regression test that drives a modal-open → modal-close sequence
through the TestBackend and asserts the post-close buffer has no
`│` cells outside the input box.

### 2. Markdown tables render as raw pipe syntax

**Repro**: any response that includes a `| Col | Col |` /
`|---|---|` block. npcterm shows literal pipes instead of a
grid.

**Root cause**: `cc-tui/src/markdown.rs::render_markdown` has
handlers for fenced code, headings, blockquotes, rules, but no
`|`-table detector. Pipe rows fall through to `render_block_line`
and get treated as plain text.

**Fix sketch**: 2-pass — detect a table block (header row + `---`
separator + body rows), rewrite each cell into fixed-width span
chunks with `│` separators and a `─` rule between header and
body. Skim comrak's or pulldown-cmark's AST if we want a fuller
parser, but a hand-rolled detector covers 90% of what shows up in
chat. Also land a snapshot test of a small table so the render
doesn't regress quietly.

### 3. STDERR rendered with a green `✓` success glyph

**Repro**: ask the model to run a Bash command that fails
(e.g. `ls /does/not/exist`). Tool-result card shows:
```
⏺ Bash(ls /does/not/exist)
  ✓ STDERR:
    ls: /does/not/exist: No such file or directory
    Exit code: 1
```

**Root cause**: `push_tool_result` in `cc-tui/src/render.rs`
picks the bullet glyph independent of `is_error` / exit code.
Any stderr rendering should use `✗` + error color.

**Fix sketch**: branch on `is_error`: ✓ (dim green) for
success, ✗ (error red) otherwise. Same rule for stdout with
non-zero exit. One-line fix + regression test.

### 4. Slash palette popup has no backdrop

**Repro**: type `/` to open the palette. Terminal width 160,
palette width 48; transcript content behind the palette bleeds
through the transparent gap between the palette's left/right
borders and the terminal edges.

**Root cause**: `render_command_palette` renders a bordered
`Block` + `List` directly — it does not paint a `Clear` into the
popup rect first the way `render_permission_modal` does.

**Fix sketch**: `frame.render_widget(Clear, palette_rect)`
before the Block. Same as the modal. One-liner + visual
regression test at 160×40 asserting no pre-palette transcript
cells remain inside the palette rect.

## P1 — hierarchy / rhythm

### 5. Welcome banner eats 11 rows for a one-session-only payoff

**What's there now**: rows 22-33 on an empty-session launch are
`Welcome to Claude Code v0.1.0` + ASCII clawd + `cwd:` + `Tip:`.
Once the user has seen the ASCII art once, it's decoration.

**Direction**: keep the full banner for FIRST-EVER launch (when
`~/.claude/sessions/` is empty — proxy for "user hasn't used
this build before"), shrink to a 3-row compact form on every
subsequent launch: `Claude Code v0.1.0 · #session · /help` +
`cwd:` + blank. That returns 8 rows to the transcript. TS
original has the same problem; doesn't make it right.

### 6. Spinner row info is noise

**What's there now** (during streaming):
```
⠙ Cogitating…   79s · ↑ 688 ↓ 510 · $0.0000
```

Three complaints, each independently valid:
- `$0.0000` is always 0 in local dev (no pricing cfg); takes 8
  cells to say nothing.
- `↑ 688 ↓ 510` token counts read as debug. Move behind
  `/status` or `/cost`.
- Verb rotation ("Musing" → "Reflecting" → "Cogitating" →
  "Working") changes 4-5 times per turn; users assume the agent
  swapped identities. Lock to one verb per turn, or drop verbs
  for just `⠙ 79s`.

**Fix**: strip the cost, keep the spinner + elapsed. Expose
token/cost via `/status`. Pick the turn's verb at `start_stream`,
hold it for the duration.

### 7. Status bar separators collide with hyphens in the model name

**What's there now**: `claude-sonnet-4-6 · 0% context · session
9c00a9cf · phase3/implementation`

- Model name hyphens (`claude-sonnet-4-6`) get read in the same
  visual rhythm as the `·` separators, making the first item
  look like three fields.
- `0% context` hovers at 0% for the first several turns — reads
  like the counter is stuck.
- `session 9c00a9cf` vs just `#9c00a9c` — the word "session"
  is redundant with a familiar `#id` convention.

**Direction**: `claude-sonnet-4-6  │  #9c00a9c  │  phase3/…`.
Use `│` instead of `·` for field separators so hyphens in
content don't compete. Drop the `0% context` indicator until it
actually crosses a threshold (say 10%); show it only when
relevant.

### 8. Markdown horizontal rule is fixed 40 chars

**Repro**: any markdown response with a `---` line. At a 160-col
terminal the TUI renders a 40-char `─────────────` floating on
the left.

**Direction**: either stretch to `area.width` (with a faded
style to avoid a visual slam), or drop the render entirely.
Headings and blockquote bars already handle section separation.

### 9. Three similar vertical-bar glyphs crowd the left margin

- `⏺` — tool-call bullet
- `▎` — heading bar
- `▏` — blockquote

All three show up on the same left column during a typical
assistant response. They read as "three kinds of the same
thing" but are semantically different.

**Direction**: keep `⏺` for tool cards (tool-ness is a distinct
concept worth its own glyph). Replace heading bar `▎` with a
blank leading row + bold text, no glyph. Keep `▏` for blockquote.

## P2 — polish

### 10. Input box's 3-row border → 1-row `>` prompt

Current chrome (6 rows) could become 4 by dropping the `┌─┐ / │
/ └─┘` wrapper: single line `> <input text>` with the `>` in
claude-orange claims "this is input" by itself. Matches shell
muscle memory. Biggest single space win.

### 11. Help footer + status bar → 1 row

Two rows of chrome where both are static hints:
```
 ? for shortcuts  ·  / for commands  ·  @ for files  ·  ! for bash
claude-sonnet-4-6 · 0% context · session 9c00a9cf · phase3/implementation
```

Fold into one: `claude-sonnet-4-6  │  #9c00a9c  │  ?help /cmds
@files !bash`. Saves a row, reads faster (hint + identity in one
glance).

### 12. `Ask Claude…` ellipsis is U+2026

In most terminal monospace fonts U+2026 renders at ~0.5
char-width, so the placeholder reads as `Ask Claude.`. Use three
dots `...` or drop the ellipsis.

### 13. Cross-platform caret inconsistency

Self-painted `▌` caret + terminal native block caret coexist on
some emulators (seen intermittently in npcterm — two carets
visible). Decide: either rely entirely on `frame.set_cursor_position`
and drop the `▌` glyph, or hide the native caret and keep the
self-paint. Not both.

## Design direction

Current TUI is a **faithful port of TS Ink's visual language** —
same welcome banner, same 3-row bordered input, same
help+status-bar footer pair. TS's visual choices weren't rigorous
either; copying them forward doesn't justify them.

Alternate target: **shell-native chat**.

- Transcript permanently in scrollback (already is under
  Inline).
- Live area: 1-row input + 1-row status-hint = 2 rows of chrome.
- Everything else is keystroke-activated (`?` opens help
  overlay, `/status` shows cost+tokens+context, `/cwd` shows
  path).

Today's 6 rows of chrome → 2 rows. On a 30-minute session that's
4 extra rows of conversation visible at all times.

## Suggested ordering

If picked up incrementally, land in this order — each one
independently valuable, each one ~1 PR:

1. **P0 #1** — modal ghost + stale footer (most visible break)
2. **P0 #2** — markdown tables (output-quality ceiling)
3. **P0 #3** — STDERR ✓ (semantic error)
4. **P0 #4** — palette backdrop
5. **P1 #6** — spinner noise: strip `$0.0000`, lock verb
6. **P1 #8** — drop/stretch h-rule
7. **P2 #10** — input box single-row (biggest UX win, gated on
   1-4 first so the tighter chrome doesn't expose other bugs)

P1 #5, #7, #9 and P2 #11, #12, #13 are all nice-to-have; defer
until the 1-7 batch lands and resurveys the UI.

## Not in this critique

- Performance (frame rate, input lag) — TUI felt responsive
  during the walkthrough.
- Accessibility (screen reader, color-blind palette) — needs a
  separate pass, not covered here.
- Mouse support — TUI doesn't wire crossterm's mouse events;
  deliberate omission, not a gap.
