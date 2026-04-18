## 1. Type-Check Merge

- [x] 1.1 In `merge_json`, before replacing a key, compare value
      shapes via `discriminant`-style match (Object vs Array vs
      Scalar).
- [x] 1.2 On mismatch, return `Err(CcError::config(...))` with the
      field path and both layer names.

## 2. Known Collections

- [x] 2.1 Preserve array-concat for fields listed in a whitelist
      (`hooks`, `permissions`, `additional_contexts`).

## 3. Surfaced Error

- [x] 3.1 At settings-load time, the error names the conflicting file
      paths so the user can edit the right file.

## 4. Tests

- [x] 4.1 Unit: base `{"a":{"x":1}}` + overlay `{"a":[1,2]}` → Err.
- [x] 4.2 Unit: base `{"hooks":[h1]}` + overlay `{"hooks":[h2]}` →
      Ok(`{"hooks":[h1,h2]}`) (concat preserved).
- [x] 4.3 Unit: base `{"model":"x"}` + overlay `{"model":"y"}` →
      Ok("y") (scalar replace, same type).

## 5. Sign-off

- [x] 5.1 `cargo test -p cc-config` + clippy clean.
