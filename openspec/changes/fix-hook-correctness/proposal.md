## Why

Four gaps in `cc-hooks` diverge from the TS reference in ways that lose
data, silently drop features, or open a security hole:

1. **`asyncRewake` semantics missing.** TS `src/utils/hooks.ts:205-244`
   treats an exit-code-2 on a hook with `asyncRewake: true` as an
   async wake-up signal: the model is re-invoked with the hook's
   output as a system reminder. Rust collapses this onto the same
   `Block` path, so async wake hooks can't do their job.

2. **`additionalContext` collected but never injected.** TS
   `src/utils/hooks.ts:2783-2788` collects every `additionalContext`
   string a hook emits and splices it into the outgoing request as a
   synthetic user message. The Rust runner discards them — the
   feature is fully dead.

3. **HTTP-hook header preparation unsafe.** TS
   `src/utils/hooks/execHttpHook.ts:76-108` interpolates `${ENV_VAR}`
   in header values *and* sanitises CR, LF, NUL bytes to prevent
   HTTP-header-injection (CRLF-injection) attacks. Rust has no HTTP
   hook yet, but the eventual client will need these primitives, and
   any intermediate call that assembles headers from user-controlled
   configuration is vulnerable without them.

4. **Plugin / project env vars not exported.** TS
   `src/utils/hooks.ts:881-906` sets `CLAUDE_PROJECT_DIR`,
   `CLAUDE_PLUGIN_ROOT`, `CLAUDE_PLUGIN_DATA`, and
   `CLAUDE_PLUGIN_OPTION_<UPPER_NAME>` on every hook child process.
   Rust doesn't set any of these, so plugin-authored hooks can't
   resolve their own data directory or read their configuration.

## What Changes

- **`HookOutcome::AsyncRewake(String)`** — a new variant distinct from
  `Block`. Produced when a hook with `async_rewake: true` exits 2.
  The engine is expected to re-inject the message into the next
  prompt rather than erroring out of the current turn.
- **`HookRunResult`** — `HookRunner::run` now returns an aggregate
  struct containing (a) the outcome and (b) every `additionalContext`
  string collected across all hooks of the event. Collection persists
  across non-halting hook failures.
- **`HookContext`** — optional environment context (project dir,
  plugin root, plugin data dir, plugin options map) attached to the
  runner via `HookRunner::with_context`. Every hook child process
  inherits the mapped env vars. `CLAUDE_PLUGIN_OPTION_*` keys follow
  the TS normalisation: non-identifier chars → `_`, then uppercased.
- **`cc_hooks::http::prepare_header[s]`** — new pure helpers that
  interpolate `${NAME}` / `$NAME` against a caller-supplied env map
  (optionally filtered by an allowlist) and reject any resulting
  header name or value containing CR, LF, or NUL. Unlike TS, unset
  env vars, disallowed env vars, and control-byte values are all
  returned as `HeaderPrepError` rather than silently replaced with
  empty strings. This is the security-fix side of the port — failing
  loud prevents a mis-configured header from shipping.
- **`cc-query` consumer** — the engine drains hook-emitted
  `additional_contexts` into the outgoing message list as synthetic
  user messages before every API call, matching TS splicing
  semantics. `AsyncRewake` is currently rendered as a tool-result
  error (same delivery path as `Block`) with a TODO to route through
  a future task-notification queue; see Impact.
- **`claude-cli` binary** — sets `project_dir` on the default
  `HookContext` from `std::env::current_dir()`. Plugin fields stay
  unset until the plugin loader lands.

## Capabilities

### Modified Capabilities

- `hook-runtime` — runner now surfaces async-rewake, carries
  per-event aggregated `additional_contexts`, and exports the
  documented env var set.
- `hook-http-preparation` — pure helper for HTTP-hook header
  interpolation + sanitisation, returning typed errors for both
  unset env vars and control-byte payloads.

## Impact

- **Affected code:**
  - `rust/crates/cc-hooks/src/lib.rs` (`HookOutcome`, `HookConfig`,
    `HookRunner`, `HookRunResult`, `HookContext`).
  - `rust/crates/cc-hooks/src/http.rs` (new module).
  - `rust/crates/cc-query/src/engine.rs` (drain
    `additional_contexts`, handle `AsyncRewake`).
  - `rust/cc/src/main.rs` (attach project-dir context).
- **User-visible:**
  - Hook-authored plugins can now read `$CLAUDE_PROJECT_DIR`,
    `$CLAUDE_PLUGIN_ROOT`, `$CLAUDE_PLUGIN_DATA`, and each
    `$CLAUDE_PLUGIN_OPTION_*`.
  - Hooks emitting `{"additionalContext": "…"}` on stdout see that
    string appear as a user message in the transcript.
  - `asyncRewake` hooks route their exit-2 message as a follow-up
    message instead of a hard block.
- **Known limits / follow-ups:**
  - `cc-hooks` does not yet ship an HTTP client — the new
    `http::prepare_header[s]` helpers are pure and unused outside
    tests. Wiring `reqwest` with sandbox-proxy + SSRF plumbing is a
    separate batch.
  - `AsyncRewake` is currently surfaced to the model through the
    same `tool_result` error path as `Block`. The TS equivalent
    enqueues a "task-notification" via `queueProcessor`. cc-query
    has no notification queue; when one lands, route there instead.
  - Only `PreToolUse` fires hooks in cc-query today; the
    `additional_contexts` plumbing works for any event so the
    injection will "just work" once `SessionStart`, `SubagentStart`,
    etc. are emitted (planned Batch J).
- **Risk:** LOW for correctness — new variants are additive and
  covered by unit tests. MEDIUM for the security helpers — the
  fail-loud default differs from TS (silent strip); callers that
  want TS-exact behaviour must opt out by handling the error.
