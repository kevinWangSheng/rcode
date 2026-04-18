## 1. Type-Check Merge

- [ ] 1.1 In `merge_json`, before replacing a key, compare value
      shapes via `discriminant`-style match (Object vs Array vs
      Scalar).
- [ ] 1.2 On mismatch, return `Err(CcError::config(...))` with the
      field path and both layer names.

## 2. Known Collections

- [ ] 2.1 Preserve array-concat for fields listed in a whitelist
      (`hooks`, `permissions`, `additional_contexts`).

## 3. Surfaced Error

- [ ] 3.1 At settings-load time, the error names the conflicting file
      paths so the user can edit the right file.

## 4. Tests

- [ ] 4.1 Unit: base `{"a":{"x":1}}` + overlay `{"a":[1,2]}` → Err.
- [ ] 4.2 Unit: base `{"hooks":[h1]}` + overlay `{"hooks":[h2]}` →
      Ok(`{"hooks":[h1,h2]}`) (concat preserved).
- [ ] 4.3 Unit: base `{"model":"x"}` + overlay `{"model":"y"}` →
      Ok("y") (scalar replace, same type).

## 5. Sign-off

- [ ] 5.1 `cargo test -p cc-config` + clippy clean.
