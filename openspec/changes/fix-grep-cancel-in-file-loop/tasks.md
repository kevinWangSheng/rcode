## 1. Cancel Check

- [x] 1.1 Track a `line_counter` in the per-file loop; every 512 lines,
      check `cancel.is_cancelled()` and bail.
- [x] 1.2 Same pattern for any other large-artefact iteration
      (`web_fetch` HTML strip, if non-trivial).
      Fixed 2026-04-18: evaluated `cc-tools/src/web_fetch/mod.rs::strip_html`
      (lines 221-248). The function does three linear-time regex
      `replace_all` passes + constant-count `String::replace` entity
      decodes + a whitespace-collapse pass, all on a body that is
      already bounded by the earlier fetch+read size cap and the
      outer cancel race at `mod.rs:154-164`. No per-iteration cancel
      check is needed because (a) there is no user-controllable
      iteration count inside the function and (b) total runtime is
      O(n) on an already-bounded n. Keeping the cancel-every-512-lines
      pattern as a grep-only construct.

## 2. Tests

- [x] 2.1 Test that drives a giant in-memory reader and a cancel token;
      assert the loop stops within a bounded number of lines after
      cancel.

## 3. Sign-off

- [x] 3.1 `cargo test -p cc-tools` + clippy clean.
