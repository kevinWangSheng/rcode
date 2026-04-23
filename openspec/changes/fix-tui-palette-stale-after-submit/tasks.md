## 1. Fix

- [x] 1.1 In `crates/cc-tui/src/action.rs::update`, `AppAction::
      Submit` arm (around line 232): immediately after
      `parse(&text)` returns `Some(cmd)`, call
      `close_palette(app, None)` BEFORE
      `ctx.commands.execute(&cmd, &cmd_ctx)`. No changes to the
      outcome-match branches are required.
- [x] 1.2 Sanity-check that `SubmitUserMessage` still hits
      `app.start_stream()` AFTER our close — the sequence is
      `close_palette → execute → match → start_stream` (sets mode
      to Streaming as last write).
- [x] 1.3 Add a one-line comment at the new call site explaining
      the rationale ("keep the post-match mode valid even for
      terminal outcomes").

## 2. Tests (headless, `crates/cc-tui/tests/headless.rs`)

- [x] 2.1 `palette_closes_after_info_command`: drive the state
      machine with `/help` + Submit; assert
      `app.mode == AppMode::Input`,
      `app.palette_matches.is_empty()`,
      `app.palette_selected == 0`,
      `app.palette_original.is_none()`.
- [x] 2.2 `palette_closes_after_clear_command`: same as 2.1 but
      with `/clear`; additionally assert transcript has a
      `SystemNotice("Transcript cleared.")` item.
- [x] 2.3 `palette_closes_after_compact_command`: same with
      `/compact`; assert a `CompactBoundary` transcript item was
      pushed.
- [x] 2.4 `palette_closes_after_switch_model`: same with
      `/model gpt-4` (or whatever demo accepts); assert
      `app.status.model == "gpt-4"`.
- [x] 2.5 `palette_closes_after_unknown_command`: `/doesnotexist`
      + Submit; assert mode is `Input` and a system notice was
      pushed.
- [x] 2.6 `user_message_still_enters_streaming`: type "hello" +
      Submit; assert `app.mode == AppMode::Streaming`. (Guards the
      no-regression claim.)
- [x] 2.7 `empty_submit_is_noop`: submit "" (or "/"); assert mode
      unchanged. (The `text.is_empty()` early-return predates this
      fix; covered for completeness.)

## 3. Manual Verification

- [x] 3.1 `npcterm` PTY: launch `cc-tui-demo` at 80×24, type
      `/clear` + Enter, confirm:
      - popup is gone (no `┌ Commands ...` border at rows 16-18);
      - system notice `ⓘ  Transcript cleared.` is visible;
      - footer shows `? for shortcuts · ...` not
        `↑↓ select · ...`.
      Attach capture as `/tmp/tui_bug_evidence/bug2_after_fix.txt`.

## 4. Sign-off

- [x] 4.1 `cargo test -p cc-tui` green.
- [x] 4.2 `cargo clippy -p cc-tui --all-targets` clean.
