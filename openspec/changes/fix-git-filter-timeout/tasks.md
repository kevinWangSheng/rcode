## 1. Timeout

- [x] 1.1 Read `CC_GIT_IGNORE_TIMEOUT_MS` (default 5000) at call time.
- [x] 1.2 Wrap the child with `tokio::time::timeout`; on elapse,
      `child.kill().await.ok();` and return `vec![false; paths.len()]`.
- [x] 1.3 Emit a `warn!` naming the timeout value and the path count.

## 2. Apply Elsewhere

- [x] 2.1 Audit other git shell-outs (`is_bare_repo`, `rev-parse`,
      branch lookup) for missing timeouts; apply the same pattern.

## 3. Tests

- [x] 3.1 Stub a `git` binary that sleeps 30 s; assert the call returns
      within 6 s with a timeout warning.

## 4. Sign-off

- [x] 4.1 `cargo test -p cc-git` + clippy clean.
