## 1. Mode Preservation

- [x] 1.1 `stat` the target (if it exists) before writing.
- [x] 1.2 Write via `NamedTempFile::new_in(parent)` + `set_permissions`
      + `persist`.
- [x] 1.3 If the target did not exist, skip the permission copy (let
      umask apply).

## 2. Tests

- [x] 2.1 Pre-chmod a file to `0o755`, run `Write`, assert mode
      survives.
- [x] 2.2 Write a new file, assert default umask applies (no
      regression).

## 3. Sign-off

- [x] 3.1 `cargo test -p cc-tools` + clippy clean.
