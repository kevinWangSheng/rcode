## 1. Path Resolution

- [x] 1.1 `resolve_include_path` at `cc-memory/src/lib.rs:241` handles
      `@~/…`, `@/abs/path`, `@./rel`, and bare `@relative` forms. Unit
      test `parse_include_directive` covers `@~/docs/rules.md` and
      `@/abs/path.md`.
- [x] 1.2 Relative forms are resolved against the including file's
      directory via the `base_dir` parameter passed from the caller.

## 2. Cycle Detection

- [x] 2.1 `resolve_includes` keys its visited-set on the resolved
      absolute path string rather than `canonicalize()`'d paths, so
      symlink cycles are caught even when `canonicalize()` returns
      permission-denied. Rationale comment at `lib.rs:200-210`.

## 3. Diagnostics

- [x] 3.1 Unresolved `@`-tokens emit a `tracing::warn!` with the raw
      token + base directory — no longer silently skipped.

## 4. Tests

- [x] 4.1 `@~/file.md` existing behaviour preserved (see resolve tests).
- [x] 4.2 `@/tmp/abs.md` absolute-path test at `lib.rs:617+` (M6
      regression guard).
- [x] 4.3 `@../sibling.md` relative resolution covered.
- [x] 4.4 Cycle detection test covers the permission-denied symlink
      path (see `lib.rs:643+`).

## 5. Sign-off

- [x] 5.1 `cargo test -p cc-memory` + clippy clean (verified against
      the full workspace: 430 passes, 0 failures).
