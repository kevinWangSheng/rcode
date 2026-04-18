## Why

`cc-tools/src/read.rs:72-87` performs a size check and a read as two
independent path-based operations:

```rust
if let Ok(meta) = tokio::fs::metadata(path).await {
    if meta.len() > MAX_FILE_BYTES { return error }
}
// ...
let content = tokio::fs::read_to_string(path).await?;
```

Both calls resolve the path from scratch, so an attacker (or a
misbehaving workflow) can swap the target between the two syscalls. If a
50 MB-capped `Read` call is pointed at `/tmp/small.txt` and the path is
relinked to a 10 GB log file in the window between the stat and the
read, the cap does nothing. The process OOMs before the error bubbles
back. The 50 MB guard becomes advisory.

## What Changes

- Open the file **once** (`tokio::fs::File::open(path).await?`) and do
  both the size probe and the read through that same file descriptor.
  `file.metadata().await` on an open fd is atomic: it describes exactly
  the object we will then read.
- On size-cap violation, drop the fd without slurping, and return the
  existing helpful error message.
- Optional hardening: `open` with `O_NOFOLLOW` (via `std::os::unix::fs::
  OpenOptionsExt::custom_flags(libc::O_NOFOLLOW)`) so a surprise symlink
  is an error, not a traversal. Gated behind a config flag if surprising
  to users.
- Regression test: simulate a symlink swap between stat and read using a
  background thread; assert the cap still enforces.

## Capabilities

### Modified Capabilities
- `tool-read-size-cap`: size enforcement MUST be atomic with the read,
  by using the same file descriptor for both metadata and content.

## Impact

- **Affected code:** `cc-tools/src/read.rs`.
- **Behaviour:** end users see no change on the happy path. Attackers
  cannot bypass the cap via fs race. The error message on cap-exceeded
  is identical.
- **Risk:** LOW.
