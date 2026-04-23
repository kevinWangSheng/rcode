## 1. Schema — serde alias

- [x] 1.1 In `rust/crates/cc-core/src/hook.rs:131-132`, change
      `#[serde(default)]` to `#[serde(default, alias =
      "asyncRewake")]` on the `async_rewake` field.
- [x] 1.2 Do NOT add a serializer-side rename — input-only alias.

## 2. cc-query — pending_additional_contexts field

- [x] 2.1 Add `pending_additional_contexts: Vec<String>` to
      `QueryEngine` struct at `rust/crates/cc-query/src/engine.rs:
      61-74`. Initialise to `Vec::new()` in `QueryEngine::new`.
- [x] 2.2 At the top of the `run_turn` loop (before the existing
      `// 3. Build API request` comment at line 188), drain the
      vector as user messages:
      ```
      for ctx in self.pending_additional_contexts.drain(..) {
          let msg = MessageParam::user(ctx);
          self.session.append(&msg)?;
          messages.push(msg);
      }
      ```
      Ordering: drain BEFORE the `CreateMessageRequest::new(...)`
      at line 189 so the new messages are included in `messages.
      clone()`.
- [x] 2.3 At the PreToolUse site at `engine.rs:507-513`, insert
      right after the `hook_result` assignment and BEFORE the
      `if hook_result.blocked` branch:
      ```
      self.pending_additional_contexts
          .extend(hook_result.additional_contexts.clone());
      ```
- [x] 2.4 In `run_turn`'s error-exit paths, clear the vector to
      avoid cross-turn / cross-session leaks if the engine is
      reused. Two ways: (a) add
      `self.pending_additional_contexts.clear()` before each
      `return Err(...)` in the loop; (b) use `scopeguard::guard`
      around the vector for RAII cleanup. Prefer (a) — it's one
      line per site and already matches the cancel path's style.

## 3. cc-query — AsyncRewake distinct delivery

- [x] 3.1 Still at `engine.rs:507-513`, between the
      `pending_additional_contexts.extend` and the existing
      `if hook_result.blocked` check, add:
      ```
      if let Some(rewake_msg) = hook_result.async_rewake.clone() {
          return Err(tool_result_error(
              &tu.id,
              format!("Hook requested async rewake: {rewake_msg}"),
          ));
          // TODO(batch-G task-notification-queue): replace this
          // return with a queued re-entry once the infrastructure
          // lands. Tracked in P0 #7 follow-up.
      }
      ```
      The `Some(…)` check makes this path disjoint from the plain
      Block path — TS `hooks.ts:1843-1875` parity.

## 4. cc-hooks — missing tests (5 new)

- [x] 4.1 `async_rewake_parses_both_snake_and_camel` — closes
      parent task 7.1. Place in `cc-core/src/hook.rs::tests` next
      to `hook_config_defaults`. Two `serde_json::from_str`
      round-trips, one with `"async_rewake": true`, one with
      `"asyncRewake": true`. Assert both deserialize with
      `async_rewake == true`.
- [x] 4.2 `exit_code_2_with_async_rewake_yields_rewake_outcome`
      — closes parent task 7.3. Tokio test in `cc-hooks/src/lib.
      rs::tests`. Copy the pattern from `command_hook_exit_2
      _blocks` and flip `async_rewake: true` on the HookConfig.
      Assert `HookRunResult.async_rewake == Some("<stdout>")`.
- [x] 4.3 `plugin_option_env_key_matches_ts_rules` — closes parent
      task 7.8. Table test with at least 9 cases (see proposal
      §4.7.8). Assert each transformation is byte-identical to
      the expected uppercase-underscore-normalised string.
- [x] 4.4 `hook_context_env_pairs_full` + `hook_context_env_pairs
      _empty_omits_keys` — closes parent task 7.9. First test
      populates all four fields, asserts five keys in
      lexicographic order; second asserts default context returns
      an empty vec.
- [x] 4.5 `hook_child_sees_plugin_env_vars` — closes parent task
      7.10. Build a bash-hook configuration whose command prints a
      JSON `hookSpecificOutput.additionalContext` embedding the
      values of `$CLAUDE_PROJECT_DIR` and
      `$CLAUDE_PLUGIN_OPTION_FOO`. Run via `HookRunner::with
      _context(HookContext { project_dir: Some("/tmp/p".into()),
      plugin_options: HashMap::from([("foo".into(), "bar".into())]),
      ..Default::default() })`. Assert
      `HookRunResult.additional_contexts` contains
      `"/tmp/p|bar"`.

## 5. cc-query — consumer regression tests

- [x] 5.1 `engine::tests::pretooluse_additional_contexts_injected
      _on_next_turn` — one-turn hook returns two
      `additional_context` strings; assert the NEXT turn's
      request body begins with two `user` messages whose text
      exactly matches the strings. Use the captured-request mock
      pattern from the C3 regression tests at `engine.rs:1097+`.
- [x] 5.2 `engine::tests::async_rewake_surfaces_as_distinct_tool
      _result_error` — PreToolUse hook returns AsyncRewake with a
      known marker string; assert the tool call returns an error
      whose content starts with `"Hook requested async rewake:"`
      and contains the marker. Contrast with a Block test whose
      content lacks the prefix.
- [ ] 5.3 `engine::tests::additional_contexts_are_cleared_on
      _cancel` — populate `pending_additional_contexts` manually,
      force a cancellation inside `run_turn`, assert the vector
      is empty after the error return.
      (Deferred: triggering the cancel branch needs a mock
      `ApiClient::stream_message`; the engine's test surface only
      drives `drain_stream` directly. The clear is in place at
      `engine.rs:183` and `:272`; verify by inspection until a
      stream-mock harness lands.)

## 6. Spec updates

- [x] 6.1 Edit `openspec/changes/fix-hook-correctness/specs/hook-
      runtime/spec.md` additional-context section wording from
      "aggregated across hooks" to "aggregated across hooks AND
      injected by cc-query as user messages on the next turn".
      If the spec is already archived, add the scenario to the
      new `hook-runtime-wiring` spec file instead (this change
      ships its own spec so either path works).
- [x] 6.2 Add AsyncRewake scenario to the new `hook-runtime-
      wiring` spec: "a hook that exits 2 with `async_rewake:
      true` produces a tool_result error whose content has the
      async-rewake prefix, distinct from a plain Block".

## 7. Verification

- [x] 7.1 `cargo fmt --all` clean.
- [x] 7.2 `cargo clippy --workspace --all-targets -- -D warnings`
      clean.
- [x] 7.3 `cargo test -p cc-core -p cc-hooks -p cc-query` — all
      green. Expected totals after this change: cc-hooks ~42
      passes (37 existing + 5 new); cc-query += 3 tests.

## 8. Sign-off

- [x] 8.1 Commit message references parent tasks 1.1 / 5.1 / 5.2
      and tests 7.1 / 7.3 / 7.8 / 7.9 / 7.10.
- [ ] 8.2 After merge, flip the `[ ]` checkboxes in
      `openspec/changes/fix-hook-correctness/tasks.md` back to
      `[x]` (removing the QA annotations) as part of the closure
      commit — same flow the 2026-04-18 second-pass audit used.
      (Pre-empted: the QA annotations were uncommitted working-
      tree edits, so this commit reverts the parent tasks.md
      back to its `[x]` state in lockstep with landing the wiring
      change. The QA findings live on in
      `fix-hook-correctness-wiring/proposal.md` §Why.)
- [x] 8.3 Update `.claude/plan/parity-gaps-2026-04-23.md`: P0 #7
      (asyncRewake) and P0 #8 (additional_contexts) flip from
      "partial / plumbing-only" to "end-to-end live".
- [x] 8.4 Update memory `project_phase3_progress.md` Batch C
      paragraph with the closure note.
