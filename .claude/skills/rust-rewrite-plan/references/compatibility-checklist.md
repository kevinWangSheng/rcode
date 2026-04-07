# Compatibility Checklist — Phase 2

For each category, find the source files, read the actual format, and classify the requirement.

Classification levels:
- **MUST MATCH EXACTLY** — byte-for-byte or schema-identical, no migration acceptable
- **MUST BE READABLE** — Rust version must parse existing files, but output format can differ
- **CAN CHANGE WITH TOOL** — breaking change acceptable if a migration tool is provided

---

## Category 1: Global Settings File
- Path: `~/.claude/settings.json`
- Find schema in: source repo settings validation code
- Requirement to determine: Can users run the Rust version without modifying their existing settings?

## Category 2: Project Settings File
- Path: `.claude/settings.json`
- Find schema in: same as above
- Note: Must check if project settings inherit from global or override

## Category 3: Session / History Files
- Path: `~/.claude/projects/<hash>/<session-id>.jsonl`
- Find format in: history persistence code
- Requirement to determine: Can the Rust version resume a session created by the TypeScript version?

## Category 4: Memory File
- Path: `~/.claude/memory.md` and project memory files
- Format: Markdown with optional frontmatter
- Requirement to determine: Any structured sections that must be preserved?

## Category 5: Anthropic API Wire Format
- Relevant for: ensuring the Rust client sends requests the API accepts
- Find in: API client code (message construction, headers, beta flags)
- Note: This is an external API — Rust version must match exactly

## Category 6: MCP Protocol
- Version: JSON-RPC 2.0 (verify actual version in source)
- Find in: MCP client transport code
- Requirement: Rust MCP client must interoperate with existing MCP servers

## Category 7: Slash Command Interface
- Names and argument format of all 86+ commands
- Find in: commands directory
- Requirement to determine: Must all command names be identical? Or can aliases change?

## Category 8: Hook Script Interface
- Environment variables passed to hooks
- stdin format (if any)
- Expected exit code semantics
- Find in: hooks execution code
- Requirement: Existing user hook scripts must work without modification

## Category 9: Plugin / Skill Manifest Format
- Find in: plugin loader, skill loader
- Requirement to determine: Can existing plugins/skills be loaded by Rust version?

## Category 10: Keybindings File
- Path: `~/.claude/keybindings.json`
- Find schema in: keybindings loader
- Requirement: Existing customizations must work
