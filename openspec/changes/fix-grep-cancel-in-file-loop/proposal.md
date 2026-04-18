## Why

`cc-tools/src/grep.rs` checks the cancellation token **before** each
file's line loop:

```rust
if cancel.is_cancelled() { return Err(...); }
for line in reader.lines().map_while(Result::ok) { ... }
```

Once inside the loop there is no cancel check. A single 10 GB log file
therefore keeps running until EOF even after the user presses Ctrl+C,
defeating the `<100ms abort` behaviour promised in §4 for the broader
tool loop.

## What Changes

- Insert a cheap `cancel.is_cancelled()` check every N lines (e.g. 512
  lines — the cost is negligible vs. regex work) inside the per-file
  loop.
- On cancel, close the file cleanly and propagate
  `CcError::tool("Grep cancelled")`.
- Also apply to any other tool that iterates inside a single large
  artefact (spot-check `web_fetch` HTML strip, `write` large payloads).

## Capabilities

### Modified Capabilities
- `tool-grep-cancel`: Grep MUST observe cancellation during in-file
  iteration, not only at file boundaries.

## Impact

- **Affected code:** `cc-tools/src/grep.rs`.
- **Risk:** LOW.
