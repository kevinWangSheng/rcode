# Proposal — Finish hook-correctness: wire consumers, close schema hole, add missing tests

## Why

`fix-hook-correctness` (commit `4fa4137`, 2026-04-23) landed the
`HookContext`, `AsyncRewake` outcome variant, `additional_contexts`
aggregate, and HTTP header sanitisation. QA on 2026-04-23 verified
the HTTP module and HookContext plumbing are solid; **five tasks in
that change marked `[x]` do not match the committed code**:

1. **1.1 + 7.1** — `HookConfig.async_rewake` was supposed to get
   `#[serde(alias = "asyncRewake")]` so both casings deserialise.
   Implementation at `rust/crates/cc-core/src/hook.rs:131-132` only
   has `#[serde(default)]`. Result: any settings file using the TS
   camelCase spelling silently defaults the flag to `false`, so
   async-rewake hooks never take the async path.

2. **5.1** — `QueryEngine` was supposed to add
   `pending_additional_contexts: Vec<String>` and drain PreToolUse
   contexts into the next API call. The field does not exist in the
   struct at `rust/crates/cc-query/src/engine.rs:61-74`; the
   PreToolUse site at `engine.rs:507-513` reads `hook_result.blocked`
   and discards everything else, including
   `hook_result.additional_contexts`.

3. **5.2** — the cc-query tool path was supposed to render
   `AsyncRewake` as a distinct tool_result error (with a TODO for
   eventual task-notification-queue routing). The word
   `async_rewake` does not appear anywhere in cc-query, so the
   variant falls through whatever path `Block` takes and is
   indistinguishable.

4. **7.3 / 7.8 / 7.9 / 7.10** — four unit/e2e tests named in
   tasks.md do not exist. The `plugin_option_env_key()` helper, the
   `HookContext::env_pairs()` method, and the end-to-end child-env
   flow have zero test coverage even though the code paths are
   supposed to be locked down.

Fix the parent change's tasks.md (flip `[x]` → `[ ]` with QA
annotations — done separately in this commit's sibling edit) and
draft this change to land the missing work.

## Goal

Close every gap the QA pass surfaced, in the smallest set of edits
that keeps cc-hooks producers and cc-query consumers in sync.

## What changes

### 1. Schema — serde alias

```rust
// rust/crates/cc-core/src/hook.rs:131-132
-    #[serde(default)]
-    pub async_rewake: bool,
+    #[serde(default, alias = "asyncRewake")]
+    pub async_rewake: bool,
```

No default-value change; the flag still defaults to `false` when
absent. Serde `alias` accepts either spelling on input; output (if
anyone `to_string`s a `HookConfig`) stays snake_case, which is fine
because `HookConfig` is a read-only input struct.

### 2. cc-query — consumer for additional_contexts (PreToolUse)

Add to `QueryEngine`:

```rust
// engine.rs:61-74 struct body
pub struct QueryEngine {
    // existing fields…
    /// Contexts collected from PreToolUse hooks that must flow into
    /// the next API call as synthetic `MessageParam::user` entries.
    /// Mirrors TS `hooks.ts:2783-2788`.
    pending_additional_contexts: Vec<String>,
}
```

Initialise to `Vec::new()` in `QueryEngine::new`. Consume at the
top of the `run_turn` loop, before request construction:

```rust
// engine.rs in run_turn, above line 189:
for ctx in self.pending_additional_contexts.drain(..) {
    let msg = MessageParam::user(ctx);
    self.session.append(&msg)?;
    messages.push(msg);
}
```

Populate at the PreToolUse site:

```rust
// engine.rs:507-513, after self.hooks.run("PreToolUse", …).await
self.pending_additional_contexts
    .extend(hook_result.additional_contexts.clone());
if hook_result.blocked {
    // existing block-handling
}
```

Important ordering: populate BEFORE the `if hook_result.blocked`
check. If a hook both blocks AND returns a context, the block wins
(TS semantics), but the context is retained for the next turn
after the user sees the block error. If that is considered too
permissive, clear the vector when a block fires — flag this as an
open question (see below).

Also extend the existing `additional_contexts` consumption in
`agent_runner.rs:65-68` with a matching TODO comment pointing at
the engine's drain site so the two injection paths stay
semantically aligned.

### 3. cc-query — consumer for AsyncRewake

At the end of the PreToolUse handling in `engine.rs` (still around
lines 507-513), branch on the new field:

```rust
let hook_result = self.hooks.run("PreToolUse", &hook_input, cancel).await;

self.pending_additional_contexts
    .extend(hook_result.additional_contexts.clone());

if let Some(rewake_msg) = hook_result.async_rewake.clone() {
    // Parity with TS hooks.ts:1843-1875: async_rewake hooks surface
    // as a tool_result error today, with the message captured so a
    // future task-notification queue can promote it to a mid-query
    // re-entry. Keep the behaviour distinct from Block by tagging
    // the message prefix so users can tell them apart in logs.
    return Err(tool_result_error(
        &tu.id,
        format!("Hook requested async rewake: {rewake_msg}"),
    ));
    // TODO(batch-G task-notification-queue): replace this early
    // return with a queued re-entry once the infra lands.
}

if hook_result.blocked {
    // existing code …
}
```

The prefix `"Hook requested async rewake:"` distinguishes the log
entry from a plain block. Keep the `TODO` comment so the eventual
rewiring is easy to find.

### 4. Missing tests

Add inside `cc-hooks/src/lib.rs::tests`:

- **7.1** `async_rewake_parses_both_snake_and_camel` — deserialise
  two JSON snippets `{"async_rewake": true}` and
  `{"asyncRewake": true}`; assert both produce `HookConfig`
  values with `async_rewake == true`.

- **7.3** `exit_code_2_with_async_rewake_yields_rewake_outcome`
  — tokio test. Build a `HookRunner` with a single hook whose
  config has `async_rewake: true` and a command that exits with
  code 2 printing `"please rewake now"`. Run it. Assert the
  `HookRunResult.async_rewake == Some("please rewake now")` and
  that no additional hooks would run (it's short-circuiting).

- **7.8** `plugin_option_env_key_matches_ts_rules` — table test
  calling `plugin_option_env_key` over:
  ```
  "foo"       → "CLAUDE_PLUGIN_OPTION_FOO"
  "foo-bar"   → "CLAUDE_PLUGIN_OPTION_FOO_BAR"
  "foo.bar"   → "CLAUDE_PLUGIN_OPTION_FOO_BAR"
  "foo bar"   → "CLAUDE_PLUGIN_OPTION_FOO_BAR"
  "Foo_BAR"   → "CLAUDE_PLUGIN_OPTION_FOO_BAR"
  "123foo"    → "CLAUDE_PLUGIN_OPTION_123FOO"
  "foo123"    → "CLAUDE_PLUGIN_OPTION_FOO123"
  "fo/ø"      → "CLAUDE_PLUGIN_OPTION_FO__"      (unicode → _)
  ""          → "CLAUDE_PLUGIN_OPTION_"
  ```
  Adjust the expected strings to match the exact TS regex rule
  `/[^A-Za-z0-9_]/g` (non-alphanumeric/underscore bytes become
  `_`; the identifier is then uppercased). Confirm behaviour by
  re-reading `src/hooks.ts:884-925` if any case looks wrong.

- **7.9a** `hook_context_env_pairs_full` — populate all four
  `HookContext` fields (project_dir, plugin_root, plugin_data,
  two plugin_options entries), call `env_pairs()`, assert the
  result is exactly:
  ```
  [("CLAUDE_PROJECT_DIR", …),
   ("CLAUDE_PLUGIN_ROOT", …),
   ("CLAUDE_PLUGIN_DATA", …),
   ("CLAUDE_PLUGIN_OPTION_<sorted_first>", …),
   ("CLAUDE_PLUGIN_OPTION_<sorted_second>", …)]
  ```
  Keys sorted lexicographically so test order is deterministic.

- **7.9b** `hook_context_env_pairs_empty_omits_keys` — default
  `HookContext`, call `env_pairs()`, assert the result is empty.

- **7.10** `hook_child_sees_plugin_env_vars` — tokio test. Build
  a bash hook command that runs:
  ```
  bash -c 'printf "{\\"hookSpecificOutput\\": {\\"additionalContext\\": \\"%s|%s\\"}}" "$CLAUDE_PROJECT_DIR" "$CLAUDE_PLUGIN_OPTION_FOO"'
  ```
  Wrap in a `HookContext` with `project_dir = "/tmp/p"` and
  `plugin_options = {"foo": "bar"}`. Run the hook. Assert the
  `HookRunResult.additional_contexts` vector contains
  `"/tmp/p|bar"`. This proves the env keys reach the child
  process correctly.

### 5. cc-query regression tests

- `engine::tests::pretooluse_additional_contexts_injected_on_next_turn`
  — one-turn stub hook returns two `additional_context` strings;
  after the tool call succeeds, the next API request body begins
  with two synthetic user messages whose content matches the
  strings, followed by the normal messages.

- `engine::tests::async_rewake_surfaces_as_distinct_tool_result_error`
  — PreToolUse hook returns `AsyncRewake("wake up")`; the tool
  call returns `Err(ToolResultBlock)` whose content contains the
  literal `"async rewake"` prefix and the message text. Confirm
  the error is distinguishable from a plain `Block` outcome.

## Impact

- **Affected specs**:
  - `fix-hook-correctness/specs/hook-runtime` gains a matching
    `Additional-Context Aggregation` scenario that now has a
    producer-side AND consumer-side test. Update the spec's
    wording from "aggregated" to "aggregated AND injected".
  - New `hook-runtime-wiring` cap (this change) adds scenarios for
    the PreToolUse consumer path and the distinct AsyncRewake
    delivery path.
- **Affected crates**:
  - `cc-core` — one-line serde attribute fix.
  - `cc-hooks` — five new tests; no behaviour change to HookRunner.
  - `cc-query` — new field + drain loop + PreToolUse consumer.
- **Wire / API compatibility**: additive. Existing hook settings
  files continue to work. Existing hooks that emit
  `additional_context` now have their contexts actually injected
  (which is the intended fix) — no opt-out flag.

## Open questions

1. If a hook BOTH blocks AND emits an `additional_context`, should
   the engine retain the context for the next turn or drop it?
   Current proposal: **retain**, because the block error is
   transient (user may override via `--bypass-permissions` and
   retry) and the context carries information the model might still
   need. If retention feels wrong, change the order: populate the
   vector AFTER the block check so blocked hooks never contribute
   contexts.

2. `pending_additional_contexts` lives on the engine across turns.
   Should a turn boundary clear it in failure cases (e.g., after
   `CcError::Cancelled`)? Propose: clear in the error path via a
   `scopeguard::guard(|| self.pending_additional_contexts.clear())`
   at the top of `run_turn` — defensive, avoids leaks across
   sessions when the engine is reused.

3. The AsyncRewake prefix `"Hook requested async rewake:"` is
   English-only. Fine for now; matches TS which is also English-
   only.
