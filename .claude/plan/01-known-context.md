# Known Context

## Primary Motivation
Performance improvement, stronger type safety, and learning Rust through a real-world project.
All three motivations are equally weighted — this is not purely a production migration.

## Non-Negotiable Constraints
- Must run on macOS first (primary target platform)
- Other platforms (Linux, Windows) are deferred — not required for initial completion

## Confirmed Decisions
- None. No prior architectural decisions exist. All decisions are open.

## Open Decisions

> **Reconciled 2026-04-09.** All 7 decisions resolved. See
> `.claude/plan/phase2-entry.md` for full rationale.

- ~~Async runtime~~ → **DECIDED: Tokio** (Decision 1, 2026-04-09)
- ~~TUI library~~ → **DECIDED: Ratatui + crossterm** (Decision 2, 2026-04-09)
- ~~Crate workspace structure~~ → **DECIDED: 14 crates + 1 binary** (Decision 3, 2026-04-09)
- ~~Which 80% of features~~ → **DECIDED: explicit in/out-of-scope lists** (Decision 4, 2026-04-09)
- ~~Strangler Fig vs Big Bang~~ → **DECIDED: Hybrid (Phased Cutover)** (Phase 1, `05-strategy-decision.md`)
- ~~MCP transport approach~~ → **DECIDED: stdio + sse + http + mTLS** (Decision 5, 2026-04-08)
- ~~Auth/OAuth implementation~~ → **DECIDED: macOS Keychain + ANTHROPIC_API_KEY** (implemented in spike)
- ~~Multi-agent / task scope~~ → **DECIDED: 4 stable task types** (Decision 6, 2026-04-08)
- ~~Tokenizer parity~~ → **DECIDED: reuse usage.input_tokens + rough delta** (Decision 7, 2026-04-09)
- ~~Multi-agent UDS IPC~~ → **DELETED: factual error** (Decision 6, 2026-04-08)

## Definition of "Complete"
80% feature parity with the current TypeScript Claude Code implementation.
Considered done when the Rust binary can serve as a daily driver for the core workflow:
- Multi-turn conversation with Claude API (streaming)
- Tool use (Bash, Read, Write, Edit, Glob, Grep, WebFetch, WebSearch)
- Session persistence and resume
- MCP server support
- Hooks system
- Slash commands (core set)
- Permission system

Not required for completion:
- 100% edge case coverage
- Windows/Linux support
- Plugin system (full)
- Every slash command

## Known Exclusions
No specific exclusions identified. User has no known problematic patterns from the existing
TypeScript code that must be avoided. Architecture is free to be redesigned as appropriate.
