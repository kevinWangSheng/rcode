## Why

`cc-session/src/lib.rs` contains two startup-path panics:

- Line 55: `fs::create_dir_all(path.parent().unwrap())` — panics if
  `transcript_path` ever returns a path with no parent (e.g., corrupt
  `dirs::home_dir()` returning `/`).
- Line 189: `impl Default for Session { Session::new().expect("...") }`
  — panics on the same class of failure. Any code path that touches
  `Session::default()` via a stray `..Default::default()` in tests or
  shared structs brings the whole CLI down.

Neither panic is defensive; both are "we don't expect this" that become
"CLI never starts" in the unlucky environments where it happens (home
mounted from a network drive that is temporarily unavailable, running
as a user with no home, etc.).

## What Changes

- Replace `.unwrap()` on `path.parent()` with `.ok_or_else(|| CcError::
  io("invalid transcript path"))?`.
- Remove `impl Default for Session` entirely (it is not used in
  production code; a check of callers confirms that only tests touch
  it, and tests can use an explicit fixture helper).
- If removal is not feasible, replace the `expect` with a clear
  `panic!("Session::default is a test-only helper; use Session::new()")`
  and scope it behind `cfg(test)`.

## Capabilities

### Modified Capabilities
- `session-lifecycle`: Session construction MUST surface errors as
  `CcResult`; no panic paths reachable from user input or environment
  state.

## Impact

- **Affected code:** `cc-session/src/lib.rs`.
- **Risk:** LOW.
