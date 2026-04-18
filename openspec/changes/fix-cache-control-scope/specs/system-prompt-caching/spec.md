## MODIFIED Requirements

### Requirement: Three-Tier Cache Control on System Blocks

The claude-cli binary and the query engine SHALL tag every system prompt
block with the `cache_control` scope that matches its lifetime tier, in
order to match the Anthropic wire contract documented in
`RUST_REWRITE_PLAN.md` §3.

Three tiers are defined:

- **Attribution (uncached):** the short "You are Claude Code …" prefix.
  This block MUST NOT carry a `cache_control` field.
- **Static (global cache):** instruction text that is stable across all
  sessions and projects. This block MUST carry
  `cache_control = { type: "ephemeral", scope: "global" }`.
- **Dynamic (org cache):** project-scoped context — git branch/commits,
  loaded memory files, CLAUDE.md content, and similar per-workspace
  material. Each such block MUST carry
  `cache_control = { type: "ephemeral", scope: "org" }`.

No system block SHALL be emitted with `cache_control = { type: "ephemeral",
scope: None }`. The `None` scope value is reserved for the "no cache_control
at all" case and MUST be modeled by omitting the field entirely.

#### Scenario: Attribution block has no cache_control
- **WHEN** `build_system_blocks` assembles the outbound `system` array
- **THEN** the first block's `cache_control` field is absent (serde skips it)

#### Scenario: Static instruction block is global-cached
- **WHEN** `build_system_blocks` emits the static instruction block
- **THEN** the block carries `cache_control.type == "ephemeral"` and
  `cache_control.scope == "global"`

#### Scenario: Git / memory blocks are org-cached
- **WHEN** the git context or a memory-file block is emitted
- **THEN** each block carries `cache_control.type == "ephemeral"` and
  `cache_control.scope == "org"`

#### Scenario: Second turn hits the cache
- **GIVEN** a session that has already sent one turn with the three-tier
  tagging
- **WHEN** the user sends a second turn
- **THEN** the API response `usage.cache_read_input_tokens` is greater
  than zero, demonstrating that global and/or org cache entries were
  retrieved (this is the end-to-end health check for this requirement).
