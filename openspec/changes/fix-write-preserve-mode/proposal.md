## Why

`cc-tools/src/write.rs` uses `tokio::fs::write(path, content).await`,
which opens the file with the default umask (typically 0644) when
creating. When overwriting an existing file, whether the mode is
preserved depends on tokio / OS semantics but in practice is not
guaranteed. A `chmod +x` script written via the `Write` tool can come
back with 0644 and stop being executable.

Worse, this surprise shows up silently — a script edited through a
coding assistant suddenly stops working, and the user has to `chmod +x`
manually to recover.

Overlaps with `fix-edit-atomic-write` (same pattern needed there).

## What Changes

- Before writing, capture the existing mode via `metadata(path)`.
- Write through a same-directory tempfile (mirrors the Edit fix) +
  `set_permissions(mode)` on the tempfile + atomic rename.
- If the file does not exist, use the default umask (new file — no
  mode to preserve).

## Capabilities

### Modified Capabilities
- `tool-write-mode`: `Write` MUST preserve the file mode of an
  existing target.

## Impact

- **Affected code:** `cc-tools/src/write.rs`.
- **Risk:** LOW.
