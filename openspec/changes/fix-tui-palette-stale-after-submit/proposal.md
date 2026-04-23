# Bug Report — Command palette stays open with stale matches after submitting a terminal slash command

## Summary

Submitting a slash command that does NOT promote the TUI to
`AppMode::Streaming` (e.g. `/clear`, `/compact`, `/config`,
`/switch-model`, `/reload-keybindings`, `/help`) leaves the
`AppMode::CommandPalette` popup visible on screen with stale
matches. The help footer still shows palette hints
(`↑↓ select · tab/enter accept · esc cancel`) even though the user
is effectively back to free-text composing. Only Esc (or
backspacing into a non-`/` buffer) recovers — undiscoverable.

## Environment

- **Repo commit:** `f9fd015aaa86e64c846bdf99d4ad402789be8348`
  (branch `phase3/implementation`)
- **Binary:** `target/debug/cc-tui-demo` (cc-tui 0.1.0)
- **Date:** 2026-04-22
- **Terminal:** 80 cols × 24 rows, via `npcterm` MCP
- **OS:** macOS (Darwin 25.1.0)

## Severity / Priority

- **Severity:** Moderate. The TUI is still operable after Esc,
  but any user who learns `/clear` first encounters a UI that
  appears broken (system-notice "Transcript cleared." is occluded
  by the popup; footer hints contradict mode).
- **Priority:** Medium. Hit by every user who uses `/clear`,
  `/compact`, `/config`, `/model`, `/status`, `/help`, etc.

## Preconditions

- `cc-tui-demo` launched with a no-op script
  (`CC_TUI_DEMO_SCRIPT=/tmp/tui_script_1.json`,
  `[{"sleep_ms":300}]`).
- TUI at the welcome screen, `AppMode::Input`, empty buffer.

## Reproduction Steps

1. Launch `cc-tui-demo` in an 80×24 terminal.
2. Type `/clear`.
3. Observe: palette popup appears (mode flipped to CommandPalette,
   expected).
4. Press Enter.
5. Read the screen and the help footer.

## Expected Result

- Step 5:
  - Palette popup is gone.
  - Mode is `AppMode::Input` (not CommandPalette).
  - System notice "ⓘ Transcript cleared." is visible in the
    transcript area.
  - Help footer reads:
    `? for shortcuts · / for commands · @ for files · ! for bash`.

## Actual Result

- Step 5:
  - Palette popup STILL visible at rows 16-18, showing stale
    match `/clear`.
  - Mode is STILL `AppMode::CommandPalette` (inferred from
    footer).
  - System notice is occluded by the popup (it IS in the
    transcript — confirmed by pressing Esc afterwards, the notice
    then appears at row 16).
  - Help footer reads:
    `↑↓ select · tab/enter accept · esc cancel`.

## Evidence (verbatim PTY capture)

`/tmp/tui_bug_evidence/bug2_after_clear_submit.txt`:

```
16 ┌ Commands ──────────────────────────────────────┐
17 │ /clear                                         │
18 └────────────────────────────────────────────────┘
19 ┌──────────────────────────────────────────────────────────────────────────────┐
20 │>                                                                             │
21 └──────────────────────────────────────────────────────────────────────────────┘
22  ↑↓ select  ·  tab/enter accept  ·  esc cancel
```

After Esc, the hidden notice appears:

```
16 ⓘ  Transcript cleared.
17
18
19 ┌──────────────────────────────────────────────────────────────────────────────┐
20 │> Ask Claude…                                                                 │
21 └──────────────────────────────────────────────────────────────────────────────┘
```

The same repro also applies to `/help` (CommandOutcome::Info) and
was observed in the same session: after `/help` + Enter the palette
popup stays at rows 16-18 showing stale match `/help`, with the
help text partially visible around and under it.

## Root Cause

`crates/cc-tui/src/action.rs::update`, `AppAction::Submit` arm
(lines 232-297):

```rust
AppAction::Submit => {
    let text = app.input.trim().to_string();
    if text.is_empty() {
        return UpdateResult::Continue;
    }
    app.clear_input();                       // <-- clears buffer only

    if let Some(cmd) = parse(&text) {
        ...
        let outcome = ctx.commands.execute(&cmd, &cmd_ctx);
        match outcome {
            CommandOutcome::Info(msg)       => app.push_system(msg),
            CommandOutcome::Clear           => { app.transcript.clear(); ... }
            CommandOutcome::Compact         => { app.push_compact_boundary(); }
            CommandOutcome::SwitchModel(n)  => { ... }
            CommandOutcome::ReloadKeybindings => { reload_keybindings(...); }
            CommandOutcome::Unknown(msg)    => { app.push_system(msg); }
            CommandOutcome::SubmitUserMessage(msg) => {
                app.push_user(msg.clone());
                app.start_stream();          // sets AppMode::Streaming — SAFE
                return UpdateResult::SubmitToEngine(msg);
            }
            CommandOutcome::Exit            => { ... return Quit; }
        }
    } else {
        ...
    }
}
```

Note `clear_input` in `app.rs`:

```rust
pub fn clear_input(&mut self) {
    self.input.clear();
    self.input_cursor = 0;
}
```

— it does not touch `mode`, `palette_matches`, `palette_selected`,
or `palette_original`. The palette-state helper
`close_palette(app, None)` exists in `action.rs` (line 156) and
already does the right thing — it just isn't called from the
Submit arm.

Consequence: every `CommandOutcome` variant that falls through to
the post-match code leaves the TUI in `CommandPalette` mode with
the last `palette_matches` vector untouched.

## Affected Scope / Blast Radius

- **Affected built-ins** (their `CommandOutcome` is none of
  `SubmitUserMessage` / `Exit`):
  `/help` (Info), `/version` (Info), `/cost` (Info), `/status`
  (Info), `/context` (Info), `/clear` (Clear), `/compact` (Compact),
  `/model <name>` (SwitchModel), `/config` (Info), `/mcp` (Info),
  `/hooks` (Info), `/skills` (Info), `/tasks` (Info),
  `/permissions` (Info), `/diff` (Info), `/commit` (Info),
  `/init` (Info), `/reload-keybindings` (ReloadKeybindings).
  → That is ~18 out of ~20 built-ins.
- **Unaffected:** `/plan` and `/commit`-style commands that return
  `SubmitUserMessage` (they enter Streaming and close the palette
  implicitly via `start_stream`); `/exit` (terminating).

## Fix Direction

Single-point fix in `action.rs::update`, `AppAction::Submit` arm:
call `close_palette(app, None)` BEFORE dispatching the command
outcome. This makes every branch start from
`AppMode::Input` with a cleared match list. `SubmitUserMessage`
still reaches `app.start_stream()`, which overwrites `mode` to
`Streaming` — so no conflict.

Alternative (more surgical, more error-prone): call
`close_palette` in every non-`SubmitUserMessage`/`Exit`
`CommandOutcome` arm. Rejected: per-variant checklist drifts when
new `CommandOutcome` variants are added.

## Regression Risk

- **LOW.** `SubmitUserMessage` path writes `AppMode::Streaming`
  after `close_palette` writes `AppMode::Input` — net effect
  identical to today.
- `Exit` terminates before the next draw — the palette-close
  writes are dead anyway.
- Headless tests for each affected outcome variant (see
  `tasks.md`) catch regressions if a new outcome forgets the
  default.

## Out of Scope

- Per-outcome animations / toasts. Simple immediate close is fine.
- The "palette stays open for commands that expect an argument"
  UX (e.g. `/model <name>`) — out-of-scope design question; for
  now, closing-on-submit is the observed unsurprising behaviour.

## Why (openspec field)

See Summary.

## What Changes (openspec field)

- `AppAction::Submit` in `crates/cc-tui/src/action.rs` calls
  `close_palette(app, None)` immediately after `parse(&text)`
  recognises a slash command, BEFORE dispatching
  `commands.execute`.
- No new state; no `App` field changes; no render changes.

## Capabilities

### Modified Capabilities
- `tui-command-palette`: submitting a slash command MUST close the
  palette and restore `AppMode::Input` before rendering the next
  frame. Only `SubmitUserMessage` MAY subsequently override mode
  to `Streaming` via its own `start_stream` path.

## Impact

- **Affected code:** `cc-tui/src/action.rs::update` (Submit arm).
- **Risk:** LOW — see Regression Risk above.
