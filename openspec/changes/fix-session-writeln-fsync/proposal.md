## Why

`cc-session/src/lib.rs:136-143` appends each turn to the transcript with:

```rust
let mut file = OpenOptions::new()
    .create(true)
    .append(true)
    .open(&self.transcript_path)?;
writeln!(file, "{line}")?;
```

`writeln!` writes into libc's stdio buffer (inherited through Rust's
`std::fs::File` append semantics on most platforms). There is no
`file.sync_all()` or `file.flush()` after the write. If the process is
killed — `SIGKILL`, power loss, `kill -9`, OOM killer — between the
`writeln!` returning and the OS actually flushing the page cache, the
latest turn is lost.

`RUST_REWRITE_PLAN.md` §3 ("Session transcript format: JSONL at …;
persisted after each turn") explicitly promises **per-turn** durability.
The current implementation breaks that promise.

Worse: the JSONL loader tolerates a truncated trailing line (the recent
"244c1f4 cc-session: tolerate malformed JSONL tail on resume" fix). That
tolerance was written assuming the **tail** is truncated — but if
multiple turns' worth of buffered output is lost, the last surviving line
might be an assistant turn whose matching user turn and tool results are
gone, yielding a nonsensical resumed conversation. fsync is the fix; the
tail-tolerance stays as a belt-and-braces backstop.

## What Changes

- Add `file.sync_all()` (Linux/macOS `fsync`) after the `writeln!`.
- Exit path: if `sync_all` fails (e.g., disk full, read-only filesystem),
  propagate the error up as `CcError::io` so the caller can decide
  (prompt the user, fall back to temp path, abort). Do not silently
  continue — a turn we cannot durably persist is exactly the situation
  we need to surface.
- Optional perf tradeoff: `fdatasync` via the `std::os::unix::fs::FileExt`
  / `rustix::fs::fdatasync` would skip metadata flush and be slightly
  faster. Acceptable only if a benchmark shows `sync_all` causing visible
  per-turn stall on slower filesystems (spinning disks, encrypted FS).
  Default is `sync_all` for now.
- Test: spawn a subprocess that appends one message, then `SIGKILL` it
  before natural exit, then reopen the transcript and verify the
  message is present.

## Capabilities

### Modified Capabilities
- `session-persistence`: each transcript append MUST be durable before
  the `append()` call returns to the caller.

## Impact

- **Affected code:** `cc-session/src/lib.rs` (the `append` method and
  any sibling metadata-writing paths if they make the same omission).
- **Perf:** one fsync per turn. Turns already involve network round-trips
  at second scale, so a few milliseconds of fsync is invisible. Only
  matters if someone writes a batch-append loop, and no such path exists.
- **Risk:** LOW. `sync_all` is standard library and well-understood.
