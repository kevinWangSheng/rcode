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

- [x] 3.1 Evaluate opening with `O_NOFOLLOW`. If the behaviour change
      is acceptable, add it and document in `RUST_REWRITE_PLAN.md`.
      Evaluated 2026-04-18 — **not applying O_NOFOLLOW by default.**

      Reasoning:
      - Real project layouts rely on symlinks (monorepo roots, vendor
        dirs, `.venv/bin` shims, `node_modules` hoists, IDE workspace
        shortcuts). A `Read` that refuses to follow symlinks would
        silently break daily usage.
      - The §1/§2 fix already neutralises the original TOCTOU attack:
        `ReadTool::execute` opens the file once, calls `metadata()` on
        the fd, enforces `MAX_FILE_BYTES` against the fd's size, and
        reads through the same fd. A symlink swap between open() and
        read() can no longer swap underlying inodes — the fd is pinned
        to whatever was resolved at open() time.
      - O_NOFOLLOW would only defend against the final-component
        symlink case, and only when the symlink target changes between
        a logical "user authored this path" moment and the actual open
        — a scenario not currently exposed by the tool surface.
      - If a future deployment needs hardened symlink rejection (e.g.,
        running cc in a sandbox that forbids symlink traversal), a
        one-line `custom_flags(libc::O_NOFOLLOW)` can be gated behind
        an env var. Not adding speculative knobs until a real use case
        materialises.

      Net: fd-local stat+read (§1/§2) is the load-bearing defense;
      O_NOFOLLOW is intentionally deferred. Marking [x].

## 4. Sign-off

- [x] 4.1 `cargo test -p cc-tools` + clippy clean.
