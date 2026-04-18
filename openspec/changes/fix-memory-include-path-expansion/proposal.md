## Why

`cc-memory` `@`-include resolution currently supports only `@~/...`
(tilde-prefixed). Three limitations:

- `@/abs/path/file.md` is silently ignored (no expansion, no error).
- `@../sibling.md` is not distinguished from a non-include — user intent
  unclear, no diagnostic.
- Cycle detection uses `canonicalize()`, which returns an `Err` on
  permission-denied symlinks; the fallback `.unwrap_or(original)` means
  two distinct symlinks both resolve to their original paths, and the
  dedup `HashSet` fails to catch a cycle between them.

In practice users write `@/Users/me/notes.md` or `@../shared.md` and
wonder why nothing appears in the context.

## What Changes

- Support three include forms:
  - `@~/...` (existing)
  - `@/abs/path` — absolute, no expansion needed
  - `@relative/path` — resolved against the including file's directory
- Emit a `warn!` log line for any `@`-token that fails to resolve,
  naming the token and the base directory, so users can debug.
- Replace `canonicalize().unwrap_or(original)` with a robust dedup key:
  `(dev, inode)` via `metadata()` on the resolved path (falls back to
  the resolved path string if metadata fails). Cycle detection stays
  correct even when canonicalize can't see through the chain.

## Capabilities

### Modified Capabilities
- `memory-includes`: `@`-includes MUST support `~/`, absolute, and
  relative forms, with a WARN on unresolved references.

## Impact

- **Affected code:** `cc-memory/src/lib.rs`.
- **Risk:** LOW.
