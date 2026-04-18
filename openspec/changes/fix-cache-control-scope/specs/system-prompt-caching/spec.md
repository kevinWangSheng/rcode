## MODIFIED Requirements

### Requirement: Three-Tier Cache Control on System Blocks

The claude-cli binary and the query engine SHALL preserve the three-tier
cache intent — attribution (uncached) → static (global cache) → dynamic
(org cache) — in the **in-memory** block ordering and via the
`CacheControl::ephemeral_global()` / `ephemeral_org()` /
`ephemeral_unscoped()` constructors.

**Wire-format constraint (observed 2026-04-17):** the Anthropic API
currently rejects any non-empty `scope` field with
`HTTP 400 invalid_request_error: system.N.cache_control.ephemeral.scope:
Extra inputs are not permitted`. As a result, `CacheControl.scope` MUST
NOT be serialized on outbound requests: every ephemeral block emits
`{"type":"ephemeral"}` on the wire regardless of its in-memory scope.
The helpers and the three-tier ordering stay in place so that when the
API begins accepting `scope`, a one-line flip (from
`skip_serializing` back to `skip_serializing_if = "Option::is_none"`)
on `CacheControl.scope` restores the three-tier wire tagging without
touching any call site.

Inbound deserialization MUST remain tolerant of a `scope` field so a
future server that starts sending it round-trips cleanly.

Block placement rules:
- **Attribution (uncached):** the short "You are Claude Code …" prefix.
  MUST NOT carry a `cache_control` field at all (in-memory or on wire).
- **Static (global cache intent):** instruction text that is stable
  across sessions. MUST be constructed with `ephemeral_global()`.
- **Dynamic (org cache intent):** per-workspace context (git, memory,
  CLAUDE.md, etc.). MUST be constructed with `ephemeral_org()`.

#### Scenario: Attribution block has no cache_control
- **WHEN** `build_system_blocks` assembles the outbound `system` array
- **THEN** the first block's `cache_control` field is absent both in-
  memory and on the serialized wire

#### Scenario: Static instruction block — in-memory scope is global
- **WHEN** `build_system_blocks` emits the static instruction block
- **THEN** in-memory `cache_control.kind == "ephemeral"` and
  `cache_control.scope == Some("global")`

#### Scenario: Git / memory blocks — in-memory scope is org
- **WHEN** the git context or a memory-file block is emitted
- **THEN** in-memory each block carries `cache_control.kind ==
  "ephemeral"` and `cache_control.scope == Some("org")`

#### Scenario: Wire format omits scope
- **WHEN** the outbound request is serialized to JSON
- **THEN** every ephemeral cache_control appears as
  `{"type":"ephemeral"}` with no `scope` key
- **AND** the API does not return 400 on the `scope` field

#### Scenario: Inbound scope is tolerated
- **GIVEN** a (hypothetical) server response containing
  `{"type":"ephemeral","scope":"global"}`
- **WHEN** the payload is deserialized into `CacheControl`
- **THEN** deserialization succeeds and `scope == Some("global")`

#### Scenario: One-line flip re-enables wire scope
- **GIVEN** a future change replacing `skip_serializing` with
  `skip_serializing_if = "Option::is_none"` on `CacheControl.scope`
- **WHEN** the outbound request is serialized
- **THEN** static blocks emit `{"type":"ephemeral","scope":"global"}`
  and dynamic blocks emit `{"type":"ephemeral","scope":"org"}`
- **AND** no other code needs to change
