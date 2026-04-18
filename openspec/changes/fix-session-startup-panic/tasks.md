## 1. Error Plumbing

- [ ] 1.1 Replace `path.parent().unwrap()` in `Session::new` with a
      `CcResult` arm.
- [ ] 1.2 Audit `cc-session` for any sibling `unwrap` / `expect` on the
      startup path and replace similarly.

## 2. Default Impl

- [ ] 2.1 Delete `impl Default for Session` or scope it to `cfg(test)`.
- [ ] 2.2 Update any test fixture that relied on `..Default::default()`
      to call a dedicated helper.

## 3. Tests

- [ ] 3.1 Unit test that sets `HOME=""` (or similar) and asserts
      `Session::new` returns `Err(CcError::Io(...))` rather than
      panicking.

## 4. Sign-off

- [ ] 4.1 `cargo test -p cc-session` + clippy clean.
