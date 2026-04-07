# Milestones — Phase 7

Derived from: Phase 1 (definition of complete), Phase 4 (HIGH risks → Spike milestones),
Phase 5 (Hybrid strategy), Phase 6 (build layers → phase groupings).

---

## Milestone 0: TUI Spike

**Crates included:** None (standalone binary, outside normal crate build)

**Deliverable:** A standalone `spike-tui` binary that proves Ratatui + Tokio streaming + keyboard
events work together on macOS. This is a go/no-go gate for the entire TUI approach.

**Entry criteria:**
- [ ] cc-core is defined (provides types the spike may reference)
- [ ] Ratatui, crossterm, Tokio added to workspace Cargo.toml

**Exit criteria (Spike acceptance criteria from Phase 4, Risk 1):**
- [ ] Streaming updates render at ≥ 30fps without tearing in macOS Terminal.app at 80 columns
- [ ] Ctrl+C during streaming stops stream within 100ms and partial text is visible in output panel
- [ ] Enter key while streaming is active is received and queued (not dropped)
- [ ] Memory usage stays flat over 100 simulated streaming turns (no accumulating allocations)
- [ ] Ratatui event loop and Tokio async runtime run together without deadlock over a 5-minute session

**Spike validation:** HIGH risk (score=27) — must pass ALL acceptance criteria before Phase 3 begins.
If Spike fails: evaluate Cursive, Tui-realm, or raw crossterm as alternatives before replanning.

**Parallel tracks:** N/A (single prototype binary)

---

## Milestone 1: Headless Core (Phase 1 Crates)

**Crates included:** cc-core, cc-config, cc-analytics, cc-auth, cc-api

**Deliverable:** A `claude` binary that accepts a prompt via stdin or `--message` flag,
sends it to the Anthropic API with streaming, and prints the response to stdout (plain text or JSON).
No TUI. No tools. Session is not persisted.

**Entry criteria:**
- [ ] Rust workspace scaffold created with all 20 crate stubs
- [ ] Anthropic API key available in environment or macOS Keychain
- [ ] `RUST_REWRITE_PLAN.md` finalized and reviewed

**Exit criteria:**
- [ ] `echo "hello" | claude --message "say hi"` prints streamed response to stdout on macOS
- [ ] `claude --message "what is 2+2" --no-tui --output json` returns valid JSON with `content` field
- [ ] First token latency < 800ms on M1 Mac measured against live Anthropic API (5-run average)
- [ ] `cargo test --workspace` passes for cc-core, cc-config, cc-auth, cc-api
- [ ] API key loaded from macOS Keychain via `keyring` crate (not just env var)
- [ ] `claude --version` prints version string
- [ ] `cargo clippy --workspace -- -D warnings` passes with zero warnings

**Parallel tracks:**
- Track A: cc-core → cc-config → cc-auth → cc-api (critical path)
- Track B: cc-analytics (parallel to Track A after cc-core)

---

## Milestone 2: Tool Execution + Session (Phase 2 Crates)

**Crates included:** cc-permissions, cc-tools, cc-hooks, cc-git, cc-mcp, cc-memory,
cc-session, cc-tasks, cc-agent, cc-query

**Deliverable:** A headless `claude` binary with full tool execution and session persistence.
Can run multi-turn conversations with Bash, Read, Write, Edit, Glob, Grep tools.
Sessions can be resumed with `--resume <id>`. MCP servers can be connected.

**Entry criteria:**
- [ ] Milestone 1 exit criteria all pass
- [ ] At least one MCP server available for integration testing (e.g., filesystem MCP server)

**Exit criteria:**
- [ ] `claude --message "list files in current dir" --no-tui` executes Bash tool and returns output
- [ ] `claude --message "read src/main.rs" --no-tui` reads file and returns content via Read tool
- [ ] Permission allow/deny rules from `.claude/settings.json` are respected (test with deny rule)
- [ ] Permission denial: Ctrl+C during tool prompt results in `is_error: true` tool_result
- [ ] Bash tool: non-zero exit code visible in tool_result content (not flagged as is_error)
- [ ] Session saved to `~/.claude/sessions/<id>/transcript.jsonl` after first turn
- [ ] `claude --resume <id>` restores previous session and conversation continues correctly
- [ ] Auto-compact triggers when token count exceeds threshold; session continues after compaction
- [ ] MCP stdio server (filesystem MCP) connects and tools are listed via `tools/list`
- [ ] At least one hook type (command hook) executes correctly on PreToolUse event
- [ ] `cargo test --workspace` passes for all Phase 2 crates
- [ ] `cargo clippy --workspace -- -D warnings` passes

**Parallel tracks:**
- Track A: cc-permissions → cc-tools
- Track B: cc-permissions → cc-hooks (parallel to Track A)
- Track C: cc-git (parallel to A and B, Layer 1)
- Track D: cc-memory (parallel to A, B, C)
- Track E: cc-session (after cc-api from Milestone 1)
- Track F: cc-mcp (after cc-tools)
- Track G: cc-query (after all tracks complete)
- Track H: cc-tasks, cc-agent (after cc-session, parallel to each other)

---

## Milestone 3: Interactive TUI (Phase 3 Crates)

**Crates included:** cc-skills, cc-plugins, cc-commands, cc-tui

**Deliverable:** A fully interactive `claude` terminal binary. Users can have multi-turn
conversations with tool use, permission dialogs, streaming output, and slash commands —
usable as a daily driver replacing the TS version for core workflows.

**Entry criteria:**
- [ ] Milestone 2 exit criteria all pass
- [ ] Milestone 0 (TUI Spike) ALL acceptance criteria passed
- [ ] At least one user skill file exists in `~/.claude/skills/` for testing

**Exit criteria:**
- [ ] `claude` launches interactive TUI in macOS Terminal.app (80 col) without visual artifacts
- [ ] Streaming response displays token-by-token; Ctrl+C stops stream within 100ms; partial text preserved in history
- [ ] Permission dialog appears for Write tool; user can Accept/Reject; Escape = Reject
- [ ] `/help` slash command displays help text
- [ ] `/memory` slash command lists memory files
- [ ] User skill files from `~/.claude/skills/` are loadable and invocable as slash commands
- [ ] `claude --continue` resumes most recent session in TUI mode
- [ ] `claude --resume <id>` resumes specific session by ID
- [ ] Session compaction: after auto-compact, TUI shows compact boundary in transcript
- [ ] Keybindings from `~/.claude/keybindings.json` are applied (test with custom binding)
- [ ] `claude` exits cleanly on Ctrl+Q or `/exit`
- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --workspace -- -D warnings` passes

**Spike validation:**
- [ ] TUI Spike acceptance criteria (from Milestone 0) all confirmed in production crate (not just spike binary)

**Parallel tracks:**
- Track A: cc-skills → cc-plugins → cc-commands (sequential — each depends on previous)
- Track B: cc-tui (can begin scaffolding after Spike passes; integrates with cc-commands at end)

---

## Milestone 4: 80% Feature Parity (Phase 4 Crates)

**Crates included:** cc-bridge, remaining analytics integration

**Deliverable:** The `claude` binary reaches 80% feature parity with the TypeScript version.
SDK/bridge mode works for non-interactive use cases. All core features from Phase 1 definition
of complete are functional.

**Entry criteria:**
- [ ] Milestone 3 exit criteria all pass
- [ ] Feature gap analysis against TS version completed (identify remaining 20% explicitly)

**Exit criteria:**
- [ ] `claude --print "hello"` (SDK/non-interactive mode) returns response and exits with code 0
- [ ] MCP HTTP SSE transport connects to a remote MCP server (not just stdio)
- [ ] All hook types work: command, prompt, http, agent hooks
- [ ] Memory files from `~/.claude/memory/` loaded and included in system prompt
- [ ] Git context included in system prompt (branch, recent commits) when in a git repo
- [ ] `claude --model claude-opus-4-6` overrides model correctly
- [ ] Settings from `~/.claude/settings.json` and `.claude/settings.json` both load and merge correctly
- [ ] Existing TS-version session files can be read and resumed by Rust version
  (format compatibility: history JSONL, metadata JSON)
- [ ] Analytics/telemetry events fire without errors (can be silent/no-op initially)
- [ ] `cargo test --workspace` passes for all 20 crates
- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] Binary size ≤ 50MB on macOS (release build with `--release`)
- [ ] `claude --help` output is human-readable and covers all major flags

**Parallel tracks:**
- Track A: cc-bridge
- Track B: analytics integration polish (parallel)

---

## Milestone Summary

| Milestone | Crates | Key Gate |
|-----------|--------|----------|
| M0: TUI Spike | (standalone binary) | Ratatui streaming + keyboard: ALL 5 criteria pass |
| M1: Headless Core | 5 crates | `claude --message "hi"` streams response, < 800ms TTFT |
| M2: Tool Execution | +10 crates | Full tool use + session resume + MCP + hooks |
| M3: Interactive TUI | +4 crates | Daily-driver interactive CLI; Spike criteria confirmed in prod |
| M4: 80% Parity | +1 crate | Session compat with TS; all hook types; SDK mode |
