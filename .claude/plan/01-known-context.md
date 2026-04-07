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
- Async runtime (Tokio vs async-std)
- TUI library (Ratatui or other)
- Crate workspace structure
- Which 80% of features to include vs. exclude
- Strangler Fig vs Big Bang strategy
- MCP transport implementation approach
- Auth/OAuth implementation

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
