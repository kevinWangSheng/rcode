## 1. Schema

- [x] 1.1 Added `HookConfig.async_rewake: Option<bool>` with
      `alias = "asyncRewake"` so both snake- and camelCase JSON
      forms deserialise (TS uses camel, Rust uses snake everywhere
      else — accept both for settings-file compatibility).
- [x] 1.2 `HookConfig::is_async_rewake()` accessor reads the field
      with a `false` default, matching TS behaviour.

## 2. Outcome + Aggregate Result

- [x] 2.1 `HookOutcome::AsyncRewake(String)` variant — produced when
      a hook with `async_rewake: true` exits 2. Short-circuits the
      hook chain (same as `Block`).
- [x] 2.2 `HookRunResult { outcome, additional_contexts }` —
      runner now returns an aggregate. `additional_contexts`
      accumulates every `additionalContext` string across all
      hooks, preserved across non-halting `Failed` outcomes.
- [x] 2.3 `HookRunResult::is_halting()` — `true` iff outcome is
      `Block` or `AsyncRewake`.
- [x] 2.4 `cc_hooks::HookRunner::run` signature changed to return
      `HookRunResult` (caller update in §5).

## 3. Hook JSON Output Parsing

- [x] 3.1 `HookJsonOutput` serde struct (private) with
      `additional_context: Option<String>`. Parses hook stdout
      with `serde_json::from_str` on exit 0; silent on parse
      failure so non-JSON stdout remains supported.

## 4. Env Var Context

- [x] 4.1 `HookContext { project_dir, plugin_root, plugin_data,
      plugin_options }` — all fields `Option` / `HashMap` so empty
      is the default.
- [x] 4.2 `HookContext::env_pairs()` — emits the TS-specified keys
      in a deterministic order (fixed keys first, then plugin
      options sorted by key).
- [x] 4.3 `plugin_option_env_key()` helper — ports TS's
      non-identifier-char → `_` + uppercase normalisation so the
      env var layout matches the TS reference byte-for-byte.
- [x] 4.4 `HookRunner::with_context(ctx)` builder + private
      `run_hook_command` threads the pairs into `Command::env`.

## 5. Consumer Wiring

- [x] 5.1 `cc_query::engine::QueryEngine` adds
      `pending_additional_contexts: Vec<String>`. PreToolUse hook
      contexts flow into it; the next loop iteration drains them
      as synthetic `MessageParam::user` entries ahead of the API
      call. Mirrors TS `hooks.ts:2783-2788`.
- [x] 5.2 `AsyncRewake` in cc-query's tool-execution path renders
      as a `tool_result` error (same delivery as `Block`) with a
      `TODO` note to route via a task-notification queue once that
      infrastructure lands. Explicitly called out in the proposal.
- [x] 5.3 `claude-cli` (`rust/cc/src/main.rs`) constructs a
      default `HookContext` with `project_dir = current_dir()` on
      every run.

## 6. HTTP Header Helpers (security)

- [x] 6.1 `cc_hooks::http` module exposes `prepare_header(name,
      value, &env, allowlist)` and `prepare_headers(&map, &env,
      allowlist)`.
- [x] 6.2 Interpolation recognises both `${NAME}` and `$NAME`
      forms with the standard `[A-Za-z_][A-Za-z0-9_]*` identifier
      rule. Unterminated `${` / non-identifier `$X` are kept as
      literal characters (bash-compatible — matches the TS regex
      `/\$\{([A-Z_][A-Z0-9_]*)\}|\$([A-Z_][A-Z0-9_]*)/` plus a
      slightly more permissive identifier class so snake_case env
      var names interpolate).
- [x] 6.3 `HeaderPrepError::UnsetEnvVar` — unset env var is a hard
      error. Diverges from TS (which substitutes empty string) on
      purpose: the roadmap ships this as a security-hardening fix
      and a silent empty-value header is almost always a bug.
- [x] 6.4 `HeaderPrepError::DisallowedEnvVar` — allowlist miss is
      a hard error, matching the expected parity surface (TS
      substitutes empty with a warning).
- [x] 6.5 `HeaderPrepError::ControlByte { field, byte }` — CR
      (`0x0D`), LF (`0x0A`), or NUL (`0x00`) anywhere in the
      prepared name or value triggers rejection. Field name
      distinguishes "name" vs "value" so error messages are
      unambiguous.

## 7. Tests

- [x] 7.1 `async_rewake_parses_both_snake_and_camel` — both JSON
      spellings deserialise.
- [x] 7.2 `exit_code_2_is_block_by_default` — exit 2 without
      opt-in still blocks (regression guard for legacy behaviour).
- [x] 7.3 `exit_code_2_with_async_rewake_yields_rewake_outcome`
      — exit 2 + `async_rewake: true` → `AsyncRewake` variant
      with the output captured.
- [x] 7.4 `additional_context_is_collected_from_stdout_json` —
      single hook, JSON stdout, one entry appended.
- [x] 7.5 `additional_contexts_accumulate_across_hooks` — two
      hooks, two entries in order.
- [x] 7.6 `non_json_stdout_is_ignored_for_additional_context` —
      free-form text stdout leaves the list empty.
- [x] 7.7 `block_short_circuits_subsequent_hooks` — a halting
      outcome stops the chain; later hooks' contexts are NOT
      collected, locking in TS parity for early-exit semantics.
- [x] 7.8 `plugin_option_env_key_matches_ts_rules` — unit test of
      the normalisation helper over mixed-case, non-identifier,
      and digit-bearing keys.
- [x] 7.9 `hook_context_env_pairs_full` / `_empty_omits_keys` —
      env-pair emission is complete and skips unset fields.
- [x] 7.10 `hook_child_sees_plugin_env_vars` — end-to-end: a
      bash hook references every CLAUDE_* var, echoes them in a
      JSON `additionalContext`, and the runner exposes them on
      `HookRunResult`. Verifies the pair construction is wired
      through `Command::env`.
- [x] 7.11 HTTP helpers:
      - `interpolate_braced_and_unbraced`
      - `interpolate_unset_is_error`
      - `interpolate_disallowed_env_var`
      - `interpolate_keeps_unterminated_dollar`
      - `interpolate_keeps_unterminated_braces`
      - `interpolate_non_identifier_after_dollar`
      - `reject_cr_in_value` / `reject_lf_in_value` /
        `reject_nul_in_value`
      - `reject_cr_in_name`
      - `happy_path_mixed_headers`

## 8. Sign-off

- [x] 8.1 `cargo check -p cc-hooks -p cc-query` clean.
- [x] 8.2 `cargo clippy -p cc-hooks -p cc-query --tests --
      -D warnings` clean.
- [x] 8.3 `cargo test -p cc-hooks -p cc-query` — 25 passes, 0
      failures across both crates.
