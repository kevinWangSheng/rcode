## 1. Atomic Open + Size Check

- [x] 1.1 Rewrite `ReadTool::execute` to open the file once with
      `tokio::fs::File::open`.
- [x] 1.2 Call `file.metadata().await` on the fd and check against
      `MAX_FILE_BYTES`. On exceed, return the existing helpful error.
- [x] 1.3 Read the content through the same fd, not by reopening the
      path.

## 2. Regression Test

- [x] 2.1 Test that spawns a thread which repeatedly relinks
      `testfile → small` / `testfile → huge` while the main task calls
      `Read`. Assert `Read` either succeeds with small content or
      returns the cap error — never OOMs.
- [x] 2.2 Unit test that the `file.metadata()` size matches the
      `read_to_string` byte count for a few fixed sizes.

## 3. Optional Symlink Hardening

- [ ] 3.1 Evaluate opening with `O_NOFOLLOW`. If the behaviour change
      is acceptable, add it and document in `RUST_REWRITE_PLAN.md`.

## 4. Sign-off

- [x] 4.1 `cargo test -p cc-tools` + clippy clean.
