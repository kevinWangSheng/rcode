## Why

`cc-config::Settings::merge` round-trips both layers through
`serde_json::Value`, then merges by object-key replacement:

```rust
let base = serde_json::to_value(self)?;      // {"a":{"x":1}}
let overlay = serde_json::to_value(other)?;  // {"a":[1,2]}
let merged = merge_json(base, overlay);       // {"a":[1,2]}
```

If base and overlay disagree on a field's type (object vs array vs
scalar), the merge silently replaces. The plan's §3 "unknown fields
preserved" promise — intended to protect users from losing custom
settings — decays into "unknown fields preserved only if their type
stayed the same across layers".

The practical cost: a project's `.claude/settings.json` writes `hooks:
[...]` (array), while the user's `~/.claude/settings.json` writes
`hooks: {...}` (object, older format). Instead of a clear error, one
shape silently wins and the user's hooks stop firing.

## What Changes

- Before the merge, compare `base[k]` and `overlay[k]` types. On
  mismatch, return `Err(CcError::config("field '{k}' has conflicting
  shapes: object in <layer_a>, array in <layer_b>"))` naming which
  layer had which shape.
- For known collection fields with defined merge semantics (e.g.
  `hooks` array-concat, `permissions` array-concat), keep the existing
  merge; the check applies only to unknown / scalar-object conflicts.
- Surface the error to the user on load with a clear pointer to the
  conflicting file path.

## Capabilities

### Modified Capabilities
- `config-merge`: type-mismatch merges MUST error, not silently
  overwrite.

## Impact

- **Affected code:** `cc-config/src/settings.rs`.
- **Risk:** MEDIUM — behaviour change. Users with broken configs will
  see a new startup error instead of broken-silently runtime.
  That's intentional but worth documenting in release notes.
