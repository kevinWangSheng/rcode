## ADDED Requirements

### Requirement: No Panic Paths in Session Construction

`Session::new` SHALL return `Err(CcError::Io { .. })` for any startup
failure (missing home directory, transcript path with no parent,
permission denied on session dir, etc.). It MUST NOT `unwrap`,
`expect`, or `panic!` on environment state.

Any `impl Default for Session` MUST NOT be reachable from non-test
code.

#### Scenario: Unwriteable home yields Err, not panic
- **GIVEN** a run with no usable home directory
- **WHEN** `Session::new()` is called
- **THEN** it returns `Err(CcError::Io(...))` whose message names the
  failure
- **AND** the process does not panic

#### Scenario: Default impl unavailable from production code
- **WHEN** production code attempts `Session::default()`
- **THEN** compilation fails (the impl is test-scoped or deleted)
