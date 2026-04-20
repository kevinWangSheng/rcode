# M5 Phase D — Official TUI Shell Port

**Owner:** cc-tui
**Branch:** `phase3/implementation` (or sub-branch `m5d/tui-shell`)
**Scope:** Port the official Claude Code (TypeScript + Ink) TUI shell to our Ratatui crate so the Rust binary looks and feels like the reference CLI across idle, streaming, tool-use, permission, and palette states.
**Non-goals:** Mouse support, image protocols (iTerm/Kitty), user-authored themes, rich MCP schema cards.

---

## 1. Why a Phase D

M5 Phase A/B/C (`c8fe5dd`, `1ac78fe`, `2563a14`) delivered **interaction-time** visuals:
- streaming spinner + markdown,
- `⏺ ToolName(preview)` cards + Edit diff,
- slash-command palette.

All 7 AC probes passed, but they were feature-scoped — each AC only activated **after** a user sent a message. The **empty startup state, the overall shell layout, and the input affordances** were never audited against the official TUI. The observable result (see `docs/screenshots/idle-2026-04-19.png`) is a large empty black box with a single session title, i.e. a functional prototype — not visual parity.

Phase D closes that specific gap by porting the shell components that render **before any message is sent** and the layout primitives that frame the existing Phase A/B/C output.

### Gap inventory (verified against `src/components/` in the extracted TS source)

| Area | Official (`src/…`) | Current cc-tui | Gap |
|---|---|---|---|
| Welcome screen | `components/LogoV2/WelcomeV2.tsx` — ASCII-art clawd + `Welcome to Claude Code vX.Y.Z` | — | missing entirely |
| cwd display | Shown as `cwd: <path>` inside welcome box | — | missing |
| Startup tips | `components/startup/` + `LogoV2/EmergencyTip.tsx` feed a rotating tip under the welcome box | — | missing |
| REPL layout | `screens/REPL.tsx` — flex column: transcript (unbordered scroll) → spinner line → PromptInput → hint footer | `render.rs` — 3 hard-split blocks (status bar 1 row / bordered transcript / 3-row bordered input) | structurally wrong |
| Input box | `components/PromptInput/PromptInput.tsx` — bordered 1-row input w/ dynamic border color, placeholder, `>` gutter via the border left side | 3-row bordered box, no gutter, static title | structurally wrong |
| Help footer | `components/PromptInput/PromptInputHelpMenu.tsx` — persistent dim hint line: `? for shortcuts • / for commands • @ for files • ! for bash` | — | missing |
| Spinner line | `components/Spinner.tsx` + `SpinnerGlyph.tsx` — spinner + verb (`Thinking…`, `Pondering…`, etc.) on its own row, between transcript and input | glyph folded into the status bar | wrong position |
| User message | `messages/UserPromptMessage.tsx` — `>` gutter + soft-gray bg | `> <text>` inline, no bg | minor |
| Assistant message | `messages/AssistantTextMessage.tsx` — no `Claude:` prefix; content flows bare with claude-orange accent on a leading `⏺` | `Claude:` header | wrong |
| Tool card | `messages/AssistantToolUseMessage.tsx` — no header; name + args in one line, result collapsed behind `ctrl+r to expand` | `⏺ Tool(preview)` + ticked result beneath | close, but mark semantics off |
| Status bar | Bottom status has `model • context-percent • cost`; the big permanent bar at the top of our TUI doesn't exist | permanent row 0 status bar | move to bottom + compress |
| Colors | `utils/theme.ts` — `claude` = rgb(215,119,87); bash=pink rgb(255,0,135); permission=blue rgb(87,105,247); success=green rgb(44,122,57); error=red rgb(171,43,63); dim=rgb(175,175,175) | mostly stock `Color::Green/Cyan/…` | wrong palette |

### Honest criteria revision

Phase A/B/C ACs (V1–V7) stay as-is — they verify features that do exist. Phase D adds ACs V8–V14 focused on **shell parity** and **empty-state rendering**; those are the ones the user will actually see first.

---

## 2. Deliverables (sub-phases)

Each sub-phase merges independently and leaves cc-tui green (`cargo test --workspace` + `cargo clippy -D warnings`). Ordered so a reviewer sees a coherent TUI after D2 lands.

### D1 — Theme & Color Palette (~150 LOC)

- New `cc-tui::theme` module with a `Theme` struct and `theme::current()` accessor.
- Port `utils/theme.ts` constants: `CLAUDE_ORANGE` (215,119,87), `BASH_PINK`, `PERMISSION_BLUE`, `SUCCESS_GREEN`, `ERROR_RED`, `DIM_GRAY`, `USER_BG_LIGHT`, `CLAUDE_BG_FILL`.
- `Color::Rgb(...)` for 24-bit terminals; auto-downgrade to `Color::Indexed` on `TERM=xterm-256color` or lower (detect via `supports-color` crate already in workspace).
- Single dark theme in D1; light + ansi-only variants deferred to a D6 polish pass.
- Replace every `Color::Green/Yellow/…` literal in `render.rs`, `diff.rs`, `markdown.rs` with `theme::current().success/…`.

**AC-V8:** `cargo test -p cc-tui theme::` — golden test asserting a `Bash` tool header renders in `rgb(255,0,135)` on true-color and in `Color::Indexed(198)` under `TERM=xterm-256color` (simulated via `TestBackend` capability).

### D2 — REPL Layout Skeleton (~250 LOC)

- Rewrite `render::render` split. New vertical layout:
  1. `Min(0)` — scrollable transcript viewport (unbordered; `Wrap { trim: false }`; pinned to bottom).
  2. `Length(1)` — spinner row (empty on idle).
  3. `Length(3)` — PromptInput (1 input row + top and bottom border).
  4. `Length(1)` — help footer hint line.
  5. `Length(1)` — status bar (model / context / cost).
- Remove the big outer `Borders::ALL` with session title. Session id moves into the status bar as a short suffix.
- Pure re-wiring: no content changes to transcript items in this PR — just new containers.

**AC-V9:** `TestBackend 80×24` snapshot of an idle `App::new(..)` has:
- no occurrence of `Claude -- session`,
- a row at `y = height-2` containing either `?` or `/` (the help hints),
- a row at `y = height-1` containing `claude-sonnet-4-6`.

### D3 — Welcome & Startup Tips (~300 LOC)

- New `cc-tui::welcome` module. Rendered into the transcript viewport **only** when `app.transcript.is_empty() && app.streaming_text.is_empty()`.
- Content, verbatim from the official welcome:
  ```
  Welcome to Claude Code v<VERSION>

       ╭──────────────────╮
       │   <clawd-ascii>  │
       ╰──────────────────╯

   cwd: <abbreviated path>
  ```
- Port the condensed 20-line clawd art from `components/LogoV2/WelcomeV2.tsx` (the dark-theme branch, lines 80–104). Store as a single `&'static str` in `welcome.rs`; reject the animated variants for now.
- A rotating tip below the box, sourced from a 12-entry `&'static [&str]` matching the official `EmergencyTip` set (`"Run /help to see commands"`, `"@-mention a file to attach it"`, `"! to drop into bash"`, etc.). Tip is picked by `(session_start_epoch_seconds / 30) % N` so it cycles without a timer.

**AC-V10:** Headless snapshot of `App::new(..)` (no messages, no streaming) at 80×24 contains the substrings `Welcome to Claude Code`, `cwd:`, and at least one of the tip strings. On a subsequent `push_user("hi")` the welcome disappears.

### D4 — PromptInput + Help Footer (~250 LOC)

- Port `components/PromptInput/PromptInput.tsx` structure:
  - 3-line bordered box; inner text area is single row for now (multiline deferred).
  - Left gutter inside the box is a space-padded `>` for the input mode, `!` for `! bash mode`, `@` once `@` is typed (shell mode + attach mode are rendered differently — we only do input mode in D4; bash/attach land in post-M5).
  - Border color reacts to mode: `DIM_GRAY` idle, `CLAUDE_ORANGE` focused while streaming, `PERMISSION_BLUE` during permission prompts.
  - Placeholder text `Ask Claude…` in dim italic when `app.input.is_empty() && mode == Input`.
- Help footer (`render_help_footer`, 1 row):
  - Idle: `? for shortcuts  ·  / for commands  ·  @ for files  ·  ! for bash`
  - Streaming: `esc to interrupt  ·  ctrl+c to cancel`
  - PermissionPrompt: `y allow  ·  a always  ·  n deny`
  - CommandPalette: `↑↓ select  ·  tab/enter accept  ·  esc cancel`
- All strings use theme dim gray; separator `·` is a literal `·` (U+00B7).

**AC-V11:** Snapshot tests for each mode show the correct hint line verbatim at row `height-2`. Bonus test: typing `>` into the input in Input mode renders with a claude-orange `>` prefix.

### D5 — Message Gutter Conventions (~200 LOC)

Port the bullet-prefixed, no-header look of the official transcript. Replace the current `> `/`Claude:`/`⏺` mix with one consistent gutter column:

- **User turn:** `>` in claude-orange, followed by the user's text. No trailing blank line — the transcript uses a 1-row spacer between turns, not between intra-turn blocks.
- **Assistant text:** no prefix; content flows bare. Markdown renderer output is unchanged from Phase A.
- **Tool use:** `⏺` in tool-specific color + ` Tool(preview)`; if expanded (default expanded for Edit/Bash, collapsed for Read/Grep/Glob), output indented 2 spaces under. Keep the `✓`/`✗` ticks but move them to the end of the tool line, not a separate row: `⏺ Bash(ls)  ✓` / `⏺ Edit(foo.rs)  ✗`.
- **System notice:** `ⓘ` in yellow + text, no leading `I`.
- **Compact boundary:** `── compacted ──` centered within the viewport width, dim italic.

**AC-V12:** Golden snapshot of a 4-turn fixture (user → assistant → Bash → Edit) compared to a committed `.snap` file. Regressions cause `cargo test` to fail with a diff output the reviewer can eyeball.

### D6 — Spinner Row + Streaming Verb (~150 LOC)

Move the spinner from the status bar to its own row (layout slot 2 from D2). Render as:

```
⠋ Thinking…   12s · ↑ 234 ↓ 1.2k · $0.0021
```

- Glyph: braille frame (already in `app.spinner_frame()`).
- Verb: cycle a 10-entry list (`Thinking…`, `Pondering…`, `Cogitating…`, …) every 3 s to mirror the official behaviour. Source list from `components/Spinner.tsx` verbs.
- Right side: elapsed seconds, running input/output token totals (already tracked in `UsageTracker`), running dollar estimate. All dim.
- Idle state: row is empty (length-1 slot keeps layout stable).

**AC-V13:** Timed test: submitting a turn causes the spinner row to render within 100 ms (re-uses the Phase A AC-V2 harness, just re-targets the assertion row). Ends within 100 ms of `finalize_turn`.

### D7 (optional polish) — Status Bar Rework (~100 LOC)

Compress the former top status bar into a single bottom line:

```
claude-sonnet-4-6 · 18% context used · session e11cf4bf · phase3/implementation
```

- `18% context used` = `usage.input / compact_threshold` from `UsageTracker`.
- Session id short-form (8 chars).
- Git branch lifted from `cc_git::GitContext` (already loaded at startup).

**AC-V14:** Idle snapshot shows the status line at `y = height-1` and the model name is present.

---

## 3. Component → Module mapping

| TS source | Port target | Notes |
|---|---|---|
| `components/LogoV2/WelcomeV2.tsx` | `cc-tui::welcome` | Only the dark-theme branch; extract the condensed clawd art into a `&'static str`. |
| `components/PromptInput/PromptInput.tsx` | `cc-tui::input` (new) | Move input rendering out of `render.rs` into its own module. Takes `&App`, returns `Paragraph` + border color. |
| `components/PromptInput/PromptInputHelpMenu.tsx` | `cc-tui::footer` (new) | 4 static string tables, one per `AppMode`. |
| `components/Spinner.tsx` + `SpinnerGlyph.tsx` | extend `cc-tui::app::App` + `cc-tui::spinner` module | Add verb cycling logic to the existing spinner state. |
| `screens/REPL.tsx` layout pieces | `cc-tui::render::render` | Rewrite the top-level `Layout::default().constraints(...)` call. |
| `utils/theme.ts` | `cc-tui::theme` (new) | All color literals live here. |
| `messages/UserPromptMessage.tsx`, `AssistantTextMessage.tsx`, `AssistantToolUseMessage.tsx` | `cc-tui::render::render_transcript` (existing) | Only the gutter/tick conventions change. |

All new modules are `pub(crate)`; nothing new in the public `cc-tui` re-export list.

---

## 4. Testing strategy

**Golden snapshots** are the primary Phase D test form — we are porting visuals, so assertions must exercise the rendered buffer, not internal state.

Snapshot framework: we already use `ratatui::backend::TestBackend` to grab a `Buffer` and stringify it. Formalise this into a `cc-tui::tests::render_golden!(fixture_name, app, 80, 24)` macro that:
1. Renders `app` into an 80×24 `TestBackend`.
2. Strips cell styling and writes the grid as UTF-8 text.
3. Diffs against a committed snapshot file under `cc-tui/tests/snapshots/<name>.snap`.
4. On `INSTA_UPDATE=1`, overwrites the snapshot instead of asserting — same convention as `insta`, but without adding the crate (the macro is ~30 LOC).

Fixtures to commit (7 files under `cc-tui/tests/snapshots/`):
- `idle_welcome.snap` — AC-V10
- `input_idle.snap`, `input_streaming.snap`, `input_permission.snap`, `input_palette.snap` — AC-V11
- `four_turn_transcript.snap` — AC-V12
- `streaming_spinner_row.snap` — AC-V13

**Runtime smoke test** (non-gating): a live 10-turn session on macOS Terminal.app, iTerm2, and WezTerm. 3 screenshots per terminal attached to the final Phase D PR. Non-gating because terminals are still varied; the snapshots are the contract.

---

## 5. Risks

- **Color downgrade bugs.** 24-bit → 256-color mapping is ad-hoc; a wrong indexed code makes Bash look brown. Mitigate: per-color unit test of the `downgrade_to_256(Rgb)` function against the official color's closest xterm index.
- **Layout reflow churn.** Changing from 3-block to 5-block layout will shift every existing headless test's row offsets. Mitigate: update all Phase A/B/C tests in the D2 PR — budget is in-scope.
- **Welcome fights with the transcript scroll anchor.** The welcome box is rendered inside the transcript viewport, so the "pin to bottom" scroll anchor needs a carve-out for the empty-transcript case. Mitigate: when `transcript.is_empty()`, disable scroll and render welcome centered.
- **Clawd ASCII art width.** Official box is 58 chars wide; terminals narrower than that break the art. Mitigate: fall back to a 3-line compact logo (`✻ Claude Code`) when `area.width < 60`.

---

## 6. Entry / Exit criteria

**Entry:**
- [x] M5 Phase A/B/C merged (HEAD ≥ `2563a14`).
- [ ] This plan doc reviewed (user signs off before D1 code starts).

**Exit (all must pass before archiving):**
- [ ] AC-V8 through AC-V14 pass (see §2).
- [ ] All pre-Phase-D headless tests still pass after layout reflow.
- [ ] `cargo test --workspace` green.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean.
- [ ] `cargo fmt --check` clean.
- [ ] `CC_TUI_MINIMAL=1` still produces the Phase-A minimal output — Phase D affordances all sit behind the same gate.
- [ ] 10-turn live smoke test on Terminal.app + iTerm2 + WezTerm, screenshots attached to the PR.
- [ ] `MEMORY.md` + `project_phase3_progress.md` updated to reflect D status.

**Rollback:**
- Each sub-phase is a single merge commit on `phase3/implementation`. `git revert <merge-sha>` restores the prior TUI. Because D1 lands new RGB colors that D2+ depend on, prefer reverting in reverse order (D7 → D1).

---

## 7. LOC + effort budget

| Phase | LOC | Risk | Depends on |
|---|---|---|---|
| D1 Theme | 150 | LOW | — |
| D2 Layout | 250 | MEDIUM (reflow) | D1 |
| D3 Welcome | 300 | LOW | D1, D2 |
| D4 Input+Footer | 250 | LOW | D1, D2 |
| D5 Gutter | 200 | LOW | D2 |
| D6 Spinner row | 150 | LOW | D2 |
| D7 Status bar | 100 | LOW | D2 |
| **Total** | **~1 400** | | |

Estimate is bounded by how much of the TS source we *re-read* — the text art, tip list, and verb list in particular — not by algorithmic work. Expect 2–3 sessions to complete.

---

## 8. Out-of-scope / follow-up candidates

- `tui-syntect` real wire-up (still scaffolded from Phase C).
- Multi-line PromptInput with soft-wrap + history navigation.
- `!` bash mode + `@` attach mode (the input gutter hook is in D4; the modes are post-M5).
- `ctrl+r` collapse/expand for tool cards.
- Light / ANSI-only theme variants.
- iTerm image protocol for image blocks.
