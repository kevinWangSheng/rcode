## 1. Path Resolution

- [ ] 1.1 Extend `resolve_include_path` to handle `@~/`, `@/abs`, and
      `@relative` forms.
- [ ] 1.2 Resolve relative forms against the including file's directory.

## 2. Cycle Detection

- [ ] 2.1 Replace `canonicalize()` + hash with `(dev, inode)`-keyed
      `HashSet` (fall back to the resolved path string if metadata
      fails).

## 3. Diagnostics

- [ ] 3.1 On unresolved `@`-token, log `warn!` with the raw token and
      the base directory.

## 4. Tests

- [ ] 4.1 `@~/file.md` — existing behaviour preserved.
- [ ] 4.2 `@/tmp/abs.md` — absolute resolves.
- [ ] 4.3 `@../sibling.md` — relative resolves against the including
      file.
- [ ] 4.4 Cycle `a.md @b.md @a.md` detected even through symlinks with
      permission-denied canonicalize.

## 5. Sign-off

- [ ] 5.1 `cargo test -p cc-memory` + clippy clean.
