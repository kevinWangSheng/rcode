## 1. Mode Preservation

- [ ] 1.1 `stat` the target (if it exists) before writing.
- [ ] 1.2 Write via `NamedTempFile::new_in(parent)` + `set_permissions`
      + `persist`.
- [ ] 1.3 If the target did not exist, skip the permission copy (let
      umask apply).

## 2. Tests

- [ ] 2.1 Pre-chmod a file to `0o755`, run `Write`, assert mode
      survives.
- [ ] 2.2 Write a new file, assert default umask applies (no
      regression).

## 3. Sign-off

- [ ] 3.1 `cargo test -p cc-tools` + clippy clean.
