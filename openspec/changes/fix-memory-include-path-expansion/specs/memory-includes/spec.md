## ADDED Requirements

### Requirement: Complete `@`-Include Forms

Memory `@`-includes SHALL support three forms: `@~/relative-to-home`,
`@/absolute/path`, and `@relative-to-including-file`. Unresolved
`@`-tokens MUST produce a `warn!` log entry naming the raw token and
the base directory, not a silent drop.

Cycle detection SHALL use a `(device, inode)` dedup key derived from
`metadata()` of the resolved path, falling back to the resolved path
string if metadata fails. It MUST NOT rely on `canonicalize()` alone,
which can fail silently on permission-denied symlinks.

#### Scenario: Absolute include
- **GIVEN** a memory file containing `@/tmp/shared.md`
- **WHEN** memory is loaded
- **THEN** `/tmp/shared.md` content is spliced in

#### Scenario: Relative include
- **GIVEN** `~/.claude/memory/a.md` containing `@sub/b.md`
- **WHEN** memory is loaded
- **THEN** `~/.claude/memory/sub/b.md` is loaded (relative to `a.md`)

#### Scenario: Unresolved include warns
- **GIVEN** a memory file containing `@does-not-exist.md`
- **WHEN** memory is loaded
- **THEN** a WARN log entry is emitted naming the token and base dir
- **AND** the raw `@does-not-exist.md` text is left in place (not
  silently dropped)

#### Scenario: Symlink cycle detected
- **GIVEN** `a.md` includes `@b.md`, `b.md` includes `@a.md`, and both
  paths are symlinks whose `canonicalize()` fails
- **WHEN** memory is loaded
- **THEN** each file is read at most once
- **AND** no infinite recursion occurs
