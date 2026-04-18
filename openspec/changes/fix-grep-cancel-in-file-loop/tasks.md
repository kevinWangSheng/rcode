## 1. Cancel Check

- [x] 1.1 Track a `line_counter` in the per-file loop; every 512 lines,
      check `cancel.is_cancelled()` and bail.
- [ ] 1.2 Same pattern for any other large-artefact iteration
      (`web_fetch` HTML strip, if non-trivial).

## 2. Tests

- [x] 2.1 Test that drives a giant in-memory reader and a cancel token;
      assert the loop stops within a bounded number of lines after
      cancel.

## 3. Sign-off

- [x] 3.1 `cargo test -p cc-tools` + clippy clean.
