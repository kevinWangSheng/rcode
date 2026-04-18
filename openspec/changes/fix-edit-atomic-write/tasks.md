## 1. Atomic Write

- [x] 1.1 Add `tempfile` to `cc-tools/Cargo.toml` runtime deps if not
      already present.
- [x] 1.2 In `EditTool::execute`, after computing `new_content`, create
      a `NamedTempFile::new_in(parent)`, write + flush + sync_all, then
      `persist(path)`.
- [x] 1.3 Preserve the pre-existing file mode on the tempfile before
      persist.

## 2. Lost-Update Detection (optional but recommended)

- [ ] 2.1 Capture `(len, mtime)` snapshot at read time.
- [ ] 2.2 Before persist, stat the path again; if the snapshot no longer
      matches, return a `ToolResult::error` asking the caller to re-read.

## 3. Tests

- [ ] 3.1 Crash test: spawn a child that starts an Edit, SIGKILL mid-way,
      reopen; file is either the old version or the new version, never
      truncated.
- [x] 3.2 Concurrency test: two threads each do an Edit on the same file;
      at least one edit lands, nothing is truncated.
- [x] 3.3 Mode preservation: pre-chmod the file to `0o755`, run Edit,
      verify mode survives.

## 4. Sign-off

- [x] 4.1 `cargo test -p cc-tools` + clippy clean.
