## 1. Helper

- [x] 1.1 Add `fn format_entry(name: &str, desc: &str) -> String`
      in `crates/cc-tui/src/commands.rs` (private). Return the
      one-line form when `name.len() <= 10`, else the two-line
      form (name on first line, description on a second line
      with leading `"              "` — 14 spaces — so it aligns
      with the short-name description column after
      `push_system_notice`'s 3-space prefix).

## 2. Call Sites

- [x] 2.1 `help_text` (line ~313): replace the built-in loop's
      `format!` with `format_entry(b.name(), b.description())`.
- [x] 2.2 `help_text` (line ~319): same substitution in the
      skills loop — use `format_entry(&s.name,
      desc_or_placeholder(&s.description))`.
- [x] 2.3 `render_skills` (line ~466): substitute — even though
      its pad is `{:<12}`, the same bug lurks for skill names >
      12 chars.

## 3. Tests (unit, `crates/cc-tui/src/commands.rs`)

- [x] 3.1 `format_entry_short`: assert
      `format_entry("help", "show this help") ==
      "  /help       show this help\n"` (byte-identical to the
      old formatter).
- [x] 3.2 `format_entry_long`: assert
      `format_entry("reload-keybindings", "re-read …") ==
      "  /reload-keybindings\n              re-read …\n"`.
- [x] 3.3 `format_entry_boundary`: assert `name.len() == 10`
      uses the one-line form; `name.len() == 11` uses the
      two-line form.

## 4. Tests (render, `crates/cc-tui/src/render.rs::tests`)

- [x] 4.1 `help_notice_no_zero_indent_continuation`: build a
      minimal `App`, `push_system(help_text)`, render to 80×30
      `TestBackend`, extract rows, assert every non-blank row of
      the notice starts with at least two spaces. Regression
      guard for the col-0 continuation.

## 5. Manual Verification

- [x] 5.1 `npcterm` PTY: launch `cc-tui-demo` at 80×24, type
      `/help` + Enter, Esc to clear palette. Read rows 14-17,
      confirm `/reload-keybindings` + description BOTH start with
      visible whitespace indent. Attach capture as
      `/tmp/tui_bug_evidence/bug3_after_fix.txt`.

## 6. Sign-off

- [x] 6.1 `cargo test -p cc-tui` green.
- [x] 6.2 `cargo clippy -p cc-tui --all-targets` clean.
