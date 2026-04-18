## Why

`cc-git::filter_git_ignored` shells out to `git check-ignore --stdin`
with no timeout. On a repo with 100k+ files, git's `check-ignore` can
take minutes; on a corrupt index it can hang indefinitely. Because this
runs synchronously in the system-prompt assembly path, a hung git call
hangs the entire CLI startup.

## What Changes

- Wrap the child in a `tokio::time::timeout` (default 5 s; configurable
  via env `CC_GIT_IGNORE_TIMEOUT_MS`).
- On timeout, kill the child, return `Vec<false>` (i.e. treat all paths
  as not-ignored), and log a `warn!` so the degradation is visible.
- This matches the "best-effort git context" pattern already used for
  `is_bare_repo`: failures degrade gracefully, they don't block the
  CLI.

## Capabilities

### Modified Capabilities
- `git-context`: git subprocess calls MUST time out rather than block
  CLI startup indefinitely.

## Impact

- **Affected code:** `cc-git/src/lib.rs`.
- **Risk:** LOW.
