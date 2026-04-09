# Rust Rewrite Plan — Phase State

| Phase | Status | Output File | Last Updated |
|-------|--------|-------------|--------------|
| 1 - Known Context | complete | .claude/plan/01-known-context.md | 2026-04-07 |
| 2 - Compatibility Contracts | complete | .claude/plan/02-compatibility-contracts.md | 2026-04-07 |
| 3 - Behavior Contracts | complete | .claude/plan/03-behavior-contracts.md | 2026-04-07 |
| 4 - Risk Inventory | complete (Risk 10 rewritten 2026-04-08) | .claude/plan/04-risk-inventory.md | 2026-04-08 |
| 5 - Strategy Decision | complete | .claude/plan/05-strategy-decision.md | 2026-04-07 |
| 6 - Dependency Graph | complete | .claude/plan/06-dependency-graph.md | 2026-04-07 |
| 7 - Milestones | complete | .claude/plan/07-milestones.md | 2026-04-07 |
| 8 - Synthesis | complete | RUST_REWRITE_PLAN.md | 2026-04-07 |
| Phase 2 Entry Gate | complete | .claude/plan/phase2-entry.md | 2026-04-09 |
| Phase 2 Detailed Design | complete | .claude/plan/phase2-design.md | 2026-04-09 |

---

## Phase 2 Detailed Design — COMPLETE (2026-04-09)

All 10 work items completed with concrete Rust type definitions:

1. **Core Type Definitions** — Message, Tool, Permission, Hook, Task types with serde contracts
2. **State & Concurrency Model** — Struct-of-Arcs AppState, CancellationToken tree, channel topology
3. **API Client Design** — cc-http (mTLS), cc-api (SSE streaming), retry/backoff
4. **Tool Loop & Query Engine** — State machine, concurrent read-only execution, auto-compact
5. **Hook Engine** — 27 events, parallel execution, structured JSON output, CLAUDE_ENV_FILE
6. **Project Discovery & Memory** — Git walk-up, 6-layer settings merge, CLAUDE.md walk-up, @-includes
7. **TUI Component Tree** — Screen layout, event routing, slash commands, streaming render
8. **Task Subsystem** — Framework + 4 task types + swarm mailbox
9. **MCP Client** — Transport abstraction (stdio/sse/http), server lifecycle, tool adapter
10. **Session Management** — JSONL transcript, TS format compat, resume/continue

**Next step:** Begin Phase 3 (implementation) starting with Layer 0 (cc-core) per the build order.

---

## Exploratory Spike Results

> **⚠️ Phase 1 scope note (2026-04-08):** The M0–M4 work captured below is
> **exploratory spike output**, produced while Phase 1 planning was still being
> synthesized. It is **not** a binding implementation of the rewrite and does
> **not** constitute Phase 2 work. Phase 2 (detailed design) has not yet begun;
> the decisions that would govern a real implementation are still open (see
> `RUST_REWRITE_PLAN.md` §2 "Open Decisions — Phase 2 Entry Gate"). Treat the
> code in `rust/` as a reference artifact, not as a shipped milestone.

| Milestone | Status | Notes |
|-----------|--------|-------|
| 0 — TUI Spike | SPIKE — exploratory only | Ratatui + Tokio streaming viability probe. 5 AC pass against `rust/spikes/tui/tests/headless.rs`. Does not bind the Phase 2 TUI-library decision. |
| 1 — Headless Core | SPIKE — exploratory only | `claude --message` streaming path wired end-to-end. Exit criteria were checked against behavior, not against a design. |
| 2 — Tool Execution + Session | SPIKE — exploratory only | Tool loop + permissions + session + MCP + hooks coded; live API smoke-test never completed. Not binding on Phase 2. |
| 3 — Interactive TUI | SPIKE — abandoned in place | ~1.2K lines across `cc-tui`; 2 runtime items never verified in Terminal.app. Do not continue — will be redesigned in Phase 2. |
| 4 — 80% Feature Parity | SPIKE — exploratory only | `--print`, HTTP SSE MCP, 4 hook kinds, TS session read — all coded against stubs. Not a parity claim. |

### Spike findings — TUI exploration (audited 2026-04-08)

> These notes document what the exploratory TUI spike covered and where it
> stopped. They are kept as reference for Phase 2 design discussions, not as a
> closeout checklist.

**Done (code present + unit tests or verifiable from code):**
- Permission dialog Accept/Reject/Escape (`cc-tui/src/lib.rs:396-423`)
- `/help` and `/memory` slash commands (`cc-commands::Builtin::{Help,Memory}` + tests)
- User skill invocable (`CommandRegistry::discover` + `render_skill` + tests)
- `--continue` / `--resume` in TUI (`cc/src/main.rs:121-141`, before TUI branch)
- Custom keybindings from `~/.claude/keybindings.json` (`cc-tui/src/keybindings.rs:60`, used at `lib.rs:156`)
- Clean exit on Ctrl+Q / `/exit` (default `kb.quit` + `CommandOutcome::Exit`)
- `cargo test --workspace` (108 tests pass) + `cargo clippy -- -D warnings` (clean) — verified 2026-04-08

**Code present, runtime unverified (need manual Terminal.app check):**
- TUI launch in 80-col without artifacts
- Streaming token-by-token + Ctrl+C <100ms abort

**Genuine gaps:**
- ~~Auto-compact boundary not rendered.~~ **FIXED 2026-04-08.** `QueryEngine::compacted_last_turn()` now tracks whether auto-compact fired during the most recent turn; the TUI `EngineDone` handler checks it and pushes a `CompactBoundary` transcript item before committing the streamed assistant text. Tests: `cc-query::tests::compact_messages_*` (2) + `cc-tui::tests::compact_boundary_rendered_before_streamed_reply`.
- TUI Spike ACs not re-confirmed in the production `cc-tui` crate. `cc-tui` has only 3 unit tests; the 10 spike headless tests live in `rust/spikes/tui/tests/headless.rs` and still need to be ported/re-verified against the production TUI.

---

## Key Files for New Sessions

| File | Purpose |
|------|---------|
| `RUST_REWRITE_PLAN.md` | Master plan — goals, scope, milestones, build order |
| `.claude/plan/implementation-notes.md` | **READ FIRST** — coding discoveries not in TS source (auth, keychain, rate limits) |
| `rust/crates/` | All 20 library crates (Phase 1–4 stubs + 5 implemented Phase 1 crates) |
| `rust/cc/` | Main binary (`claude-cli` package, `claude` binary target) |

---

## Current Codebase State — Spike Inventory

> Inventory of what the M0–M4 spikes produced under `rust/`. Listed for
> reference only; none of this is binding on the Phase 2 design.

**Implemented (non-stub):**
- `cc-core` — message/tool/permission/error types
- `cc-config` — settings load/merge + `resolve_model`
- `cc-analytics` — no-op stubs
- `cc-auth` — macOS Keychain OAuth + `ANTHROPIC_API_KEY`
- `cc-api` — SSE streaming client
- `cc-permissions` (144 lines) — allow/deny/ask rules from settings
- `cc-tools` (83 lines) — Bash/Read/Write/Edit/Glob/Grep/WebFetch/WebSearch
- `cc-hooks` (501 lines) — command/prompt/http/agent hook runner
- `cc-mcp` (784 lines across adapter/client/http_client/types/lib) — stdio + HTTP SSE MCP
- `cc-git` (138 lines) — GitContext::collect
- `cc-memory` (182 lines) — `~/.claude/memory/` loader
- `cc-session` (327 lines) — JSONL transcript, TS-format resume
- `cc-query` (472 lines across engine/permission_prompt/prompter) — tool loop + auto-compact
- `cc-bridge` (106 lines) — SDK `--print` path
- `cc-skills` (345 lines) — skill loader
- `cc-plugins` (249 lines) — plugin loader w/ bundled skills
- `cc-commands` (638 lines) — slash command registry (13 builtins + skills)
- `cc-tui` (~1.2K lines across lib/app/render/event/keybindings/prompter) — interactive TUI
- `claude-cli` binary — full CLI: `--message`, `--print`, `--resume`, `--continue`, `--model`, `--output`, `--max-tokens`, `--bypass-permissions`, `--non-interactive`, `--no-tui`, `--verbose`

**Stubs only (empty `src/lib.rs`):**
- `cc-tasks` — 1 line. Not referenced by M2 exit criteria; intentional deferral.
- `cc-agent` — 1 line. Same — deferred; not blocking any milestone.

---

## Next Work: Phase 2 Detailed Design (READY)

> **Phase 2 Entry Gate: COMPLETE (2026-04-09).** All 7 decisions resolved,
> all 4 gap patches written, spike audit done, `phase2-design.md` opened.
> See `.claude/plan/phase2-entry.md` for full records.

**Phase 2 Entry Gate — completed checklist:**

- [x] Decision 5 (MCP Transports + mTLS) — DECIDED 2026-04-08
- [x] Decision 6 (Multi-Agent / Task Subsystem) — DECIDED 2026-04-08
- [x] Gap patch A3 — project discovery — DONE 2026-04-09
- [x] Gap patch A2 — hooks lifecycle — DONE 2026-04-09
- [x] Decision 7 — Tokenizer parity — DECIDED 2026-04-09
- [x] Gap patch A1 — tasks/agent contracts — DONE 2026-04-09
- [x] Decisions 1–4 — all DECIDED 2026-04-09
- [x] B2 follow-up (Risk 11, §C1 reclassify) — DONE 2026-04-09
- [x] 01-known-context.md reconciled — DONE 2026-04-09
- [x] Spike audit (6 REUSE / 6 REWRITE / 8 DISCARD) — DONE 2026-04-09
- [x] `phase2-design.md` opened — DONE 2026-04-09

**Next step:** Phase 2 detailed design COMPLETE. Begin Phase 3 (implementation).
Start with Layer 0 (cc-core) rewrite per `phase2-design.md` §1, then Layer 1
(cc-config + cc-http) per §3 and §6.
