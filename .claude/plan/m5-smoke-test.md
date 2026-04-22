# M5 — 10-turn Terminal.app Smoke Test

Last M5 exit criterion. Run the Rust `claude` binary in macOS Terminal.app
across a scripted 10-turn session and attach screenshots to the release PR.

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

## Known deviations to expect

- Spinner glyph is a rotating braille character, not the TS CLI's `⠋`.
  This is intentional per Phase D scope.
- Status-bar session id shows the first 8 hex chars; TS shows 6.
  Intentional per D7.
- Tool-card colours use the Phase D theme palette (bash pink, edit
  yellow, read blue, grep/glob cyan, web magenta, MCP dim-gray).
  Hand-compare screenshots to verify each tool fires in its colour.
