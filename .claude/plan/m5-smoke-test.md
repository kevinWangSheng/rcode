# M5 — 10-turn Terminal.app Smoke Test

Last M5 exit criterion. Run the Rust `claude` binary in macOS Terminal.app
across a scripted 10-turn session and attach screenshots to the release PR.

## Automation status (2026-04-21, HEAD cd6766b)

The following turns have been auto-verified via the `npcterm` MCP at
120×40 against the release binary. The PTY regression tests listed at
the end of this file cover them permanently in CI.

| # | Covered | Evidence |
|---|---------|----------|
| 01 (welcome state) | ✅ auto | `pty::tests::welcome_banner_visible_under_fullscreen_fallback` + live npcterm at 2026-04-21T00:58 |
| 07 (slash palette) | ✅ auto | `/` opens palette listing builtins + skills; Tab accepts top match `/align`; Esc dismisses |
| 08 (`/help`) | ✅ auto | `/help` outputs full command + skill table |
| Ctrl+C force-quit | ✅ auto | 2× Ctrl+C returns to shell |

Turns 02–06 and 09–10 still need a live Anthropic-API session. Those
cover markdown streaming, tool cards, Edit diff, streaming overflow,
and the permission dialog — all tied to real API turns.

## Prerequisites for the manual portion

- **Refresh credentials** — the earlier session captured on 2026-04-21
  hit `HTTP 401 Invalid authentication credentials`. Run
  `./rust/target/release/claude login` and complete the browser
  OAuth flow before starting the manual run.
- Terminal.app at **120 × 40** or larger (80 × 24 works but the
  welcome banner truncates). Avoid nested tmux if possible — it
  forces the Fullscreen fallback.

## Prerequisites

- Authenticated session (`claude login` or `ANTHROPIC_API_KEY` exported).
- Release binary already built at `rust/target/release/claude`
  (rebuild with `cargo build --manifest-path rust/Cargo.toml --release
  -p claude-cli` if stale).
- Terminal.app opened at **≥ 80 cols × 30 rows** (smaller sizes are a
  separate headless check; this test is for the default daily geometry).

## How to run

Two sessions: default build, then `tui-syntect` build for AC-V7 visual
side-by-side. Note the ~1 MB binary diff per AC-V7.

```sh
# Session 1 — default build
cd /path/to/cc_src
./rust/target/release/claude

# Session 2 — syntax-highlighted build
cargo build --manifest-path rust/Cargo.toml --release -p claude-cli \
  --features cc-tui/tui-syntect
./rust/target/release/claude
```

## 10-turn script

Run each turn in order. After every turn, screenshot the full terminal
before submitting the next. Name shots `m5-smoke-NN.png`.

| # | Prompt | What to check |
|---|--------|---------------|
| 01 | *(empty-session state)* — screenshot **before** typing | welcome banner, clawd ASCII, cwd + git branch, rotating tip, `>` gutter |
| 02 | `Hello — show me **bold**, *italic*, \`code\`, and a bullet list.` | inline markdown styles visually distinct; bullet glyph `•` |
| 03 | `### Heading\nShow a fenced Rust block:\n\`\`\`rust\nfn main() { println!("hi"); }\n\`\`\`` | heading glyph `▎`; fence border `┌─ rust`; default build = plain yellow code; syntect build = multi-colour |
| 04 | `Run \`ls -la\` in my cwd with the Bash tool.` | tool card `⏺ Bash(ls -la)` in pink/green, success tick, output indented |
| 05 | `Create /tmp/m5-smoke.txt with the text "hello m5".` | `⏺ Write(/tmp/m5-smoke.txt)` card in yellow + success line |
| 06 | `Now edit that file: replace "hello" with "HELLO".` | Edit tool card + red `-`/green `+` unified diff with ≥ 2 context lines |
| 07 | Type `/` with an empty buffer | slash palette dropdown lists `/help`, `/memory`, `/clear`, user skills; Tab accepts and inserts trailing space |
| 08 | `/help` *(via palette from turn 07)* | `/help` output appears as a system-tinted transcript item |
| 09 | `Do 100 KB of random markdown prose covering headings, lists, fenced code, and inline styles.` | streaming spinner visible within ~100 ms of submit; `(+N queued)` if you type during stream; long reply wraps inside the inline viewport, older paragraphs flush to scrollback |
| 10 | Trigger a permission prompt: `Run \`rm /tmp/m5-smoke.txt\` via Bash.` (answer `N` when prompted) | framed permission dialog with `[Y] Allow`, `[A] Allow always`, `[N] Deny`; dismissing with `N` records a denied tool_result without panicking |

## What "PASS" means

Every item in the right-hand column observable in the screenshots for
**both** the default build and the `tui-syntect` build. The two builds
should differ only in turn 03's code-block colouring; every other turn
is byte-identical modulo timing.

## Archive location

Drop the two screenshot bundles (`default/` and `syntect/` subfolders)
into the release PR description. Reference them back here via commit
hash so future readers can audit the M5 exit evidence.

## Regression tests pinning this

| Test | Covers |
|---|---|
| `rust/crates/cc-tui/tests/pty.rs::smoke_demo_launches_and_renders_prompt` | banner visible at 100 × 24 Inline |
| `rust/crates/cc-tui/tests/pty.rs::welcome_banner_visible_under_fullscreen_fallback` | banner visible under Fullscreen (`CC_TUI_FORCE_FULLSCREEN=1`) |
| `rust/crates/cc-tui/tests/pty.rs::launches_cleanly_at_80_cols_no_artifacts` | welcome + `>` prompt + no escape leakage at 80 × 24 |
| `rust/crates/cc-tui/tests/headless.rs::acv5_palette_*` | palette lists builtins + skills, Tab accepts, Esc restores |
| `rust/crates/cc-tui/tests/headless.rs::ac2_abort_latency_under_100ms_end_to_end` | Ctrl+C cancels a streaming turn within 100 ms |
| `rust/crates/cc-tui/tests/headless.rs::ac4_100_turns_complete` / `ac5_no_deadlock_100_turns` | 100 consecutive turns do not leak / deadlock |
| `rust/crates/cc-tui/src/markdown.rs::tests::syntect_colors_rust_fence_with_rgb_spans` | `tui-syntect` feature produces Rgb-coloured spans |

## Known deviations to expect

- Spinner glyph is a rotating braille character, not the TS CLI's `⠋`.
  This is intentional per Phase D scope.
- Status-bar session id shows the first 8 hex chars; TS shows 6.
  Intentional per D7.
- Tool-card colours use the Phase D theme palette (bash pink, edit
  yellow, read blue, grep/glob cyan, web magenta, MCP dim-gray).
  Hand-compare screenshots to verify each tool fires in its colour.
