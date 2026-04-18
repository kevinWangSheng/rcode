## 1. Schema

- [x] 1.1 `HookConfig.command` now accepts `String | Vec<String>` via a new
      `HookCommand` enum in `cc-core/src/hook.rs` — `untagged` serde so both
      JSON forms deserialize transparently. (cc-config treats `hooks` as
      opaque JSON and forwards to cc-core unchanged.)
- [x] 1.2 Added `HookConfig.unsafe_shell: bool` (default `false`) — the
      opt-in gate for the string form.

## 2. Runner

- [x] 2.1 `cc-hooks::execute_one_hook` now branches on `HookCommand`:
      - `Argv(v)` → `run_argv_hook` → `Command::new(v[0]).args(&v[1..])`
        (no shell).
      - `Shell(s)` with `unsafe_shell: false` → `HookOutcome::Failed` with
        a message pointing the user at the migration.
      - `Shell(s)` with `unsafe_shell: true` → legacy `sh -c $s` path.
- [x] 2.2 Env-var injection (`CLAUDE_SESSION_ID`, `CLAUDE_CWD`,
      `CLAUDE_ENV_FILE`) is shared via the new `run_prepared_hook` helper,
      so both forms get identical environment plumbing.

## 3. Warning Path

- [ ] 3.1 Deferred: a consolidated startup WARN summarising all string-form
      entries is worth a follow-up, but the per-invocation `HookOutcome::Failed`
      already surfaces the problem loudly at runtime. Not load-bearing.

## 4. Tests

- [x] 4.1 `argv_hook_runs_without_shell` — `["/bin/sh", "-c", "exit 2"]`
      form runs + returns block.
- [x] 4.2 `string_command_without_unsafe_shell_is_rejected` — default
      `unsafe_shell: false` + string command → the runner records an
      `unsafe_shell`-tagged failure and does NOT execute the string.
- [x] 4.3 Existing shell-path tests (`command_hook_exit_2_blocks`,
      `once_hook_fires_only_once`, `command_hook_injects_session_env_vars`,
      etc.) updated to carry `"unsafe_shell": true` so they continue to
      exercise the legacy path.
- [x] 4.4 `parses_command_as_argv_array` + `parses_command_as_shell_string`
      lock the serde wire shape for both forms.

## 5. Sign-off

- [x] 5.1 `cargo test --workspace` (434 passes, 0 failures) + `cargo clippy
      --workspace --all-targets -- -D warnings` clean.
