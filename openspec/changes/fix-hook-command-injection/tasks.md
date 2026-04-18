## 1. Schema

- [ ] 1.1 Extend the hook settings struct in `cc-config` so `command`
      accepts `String | Vec<String>`.
- [ ] 1.2 Add optional `unsafe_shell: bool` (default `false`) per hook
      entry.

## 2. Runner

- [ ] 2.1 In `cc-hooks`, branch on the parsed form:
      - array → `Command::new(argv[0]).args(&argv[1..])`
      - string with `unsafe_shell = true` → current `bash -c` path
      - string without the flag → return an error at runner init time
- [ ] 2.2 Preserve env-var injection (`CLAUDE_SESSION_ID`, etc.) for both
      forms.

## 3. Warning Path

- [ ] 3.1 On startup, scan loaded hooks; log a single WARN summarising
      any string-form entries and the required migration.

## 4. Tests

- [ ] 4.1 Array-form unit test: `["/bin/echo", "hello"]` runs and
      returns `"hello"`.
- [ ] 4.2 String-without-flag unit test: hook loading fails with a
      clear error pointing at the field path.
- [ ] 4.3 String-with-flag unit test: legacy path still runs.

## 5. Sign-off

- [ ] 5.1 `cargo test -p cc-hooks -p cc-config` + clippy clean.
