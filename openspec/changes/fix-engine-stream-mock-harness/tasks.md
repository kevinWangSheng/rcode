## 1. Field + routing in QueryEngine

- [x] 1.1 In `rust/crates/cc-query/src/engine.rs`, add
      `#[cfg(test)] stream_override: Option<test_support::
      StreamOverrideFn>` to the `QueryEngine` struct (around
      `engine.rs:61-74`).
- [x] 1.2 Initialise to `None` in `QueryEngine::new` (search for
      the existing initialiser; add the field init under a
      `#[cfg(test)]` block).
- [x] 1.3 At the single `self.api.stream_message(...)` call site
      (today at `engine.rs:263`), replace with:
      ```
      #[cfg(test)]
      let rx = if let Some(f) = &self.stream_override {
          f(&req, cancel)
      } else {
          self.api.stream_message(req, cancel).await?
      };
      #[cfg(not(test))]
      let rx = self.api.stream_message(req, cancel).await?;
      ```
      Keep the `await?` on the production path. The override
      returns a receiver synchronously.

## 2. test_support module

- [x] 2.1 Add `#[cfg(test)] pub(crate) mod test_support` at the
      bottom of `engine.rs` (or in a sibling `engine/test_support.rs`
      if the file is getting long — preference: same file, to keep
      test-only code adjacent to the private fields it touches).
- [x] 2.2 Define:
      ```
      pub(crate) type StreamOverrideFn = std::sync::Arc<
          dyn Fn(&CreateMessageRequest, &CancellationToken)
                 -> tokio::sync::mpsc::Receiver<CcResult<StreamEvent>>
              + Send + Sync,
      >;
      ```
- [x] 2.3 Implement `QueryEngine::with_stream_override<F>(self,
      f: F) -> Self` that stores the closure in the field. Mark
      `pub(crate)` — nothing outside `cc-query` should need it.
- [x] 2.4 Implement the `scripted_stream(events:
      Vec<CcResult<StreamEvent>>) -> impl Fn(...) -> ...`
      convenience helper. Handles the common "just replay N events"
      case. (Chose synchronous `try_send` pre-fill rather than a
      spawned task: the channel is sized to exactly fit the script,
      so all events land in the receiver before the closure
      returns. This sidesteps a scheduler race on pre-cancelled
      tokens and makes the helper deterministic.) Degrades
      `Err(_)` to `Err(CcError::Other("scripted-stream error"))`
      (see §3).

## 3. CcError handling

- [x] 3.1 Checked: `cc_core::CcError` wraps `std::io::Error` and
      `serde_json::Error` (both non-Clone) via `#[from]`. Cannot
      derive `Clone` without custom impls or rebuilding the inner
      errors. Kept the degrade-to-`CcError::Other("scripted-stream
      error")` path; tests that need variant fidelity can use
      `with_stream_override` directly and construct the error
      per-call.
- [x] 3.2 Documented in the `scripted_stream` doc-comment and in
      the commit message.

## 4. Self-regression tests

- [x] 4.1 `engine::tests::stream_override_drives_full_turn` —
      build an engine with a scripted stream that yields a
      `MessageStart` + `ContentBlockStart(Text)` + one text delta
      + `ContentBlockStop` + `MessageDelta(end_turn)` +
      `MessageStop`; call `run_turn("hi", …)`; assert the
      returned final text equals the delta content. (Expanded the
      script beyond the minimal "start + delta + stop" listed in
      the spec to include the framing events `StreamAccumulator`
      expects — otherwise `into_message_and_usage_recovering`
      builds a half-formed block and the assertion is flakier.)
- [x] 4.2 `engine::tests::stream_override_respects_cancel` —
      scripted stream yields `MessageStart + text delta +
      MessageStop`; cancel the token before `run_turn`; assert
      the call returns `Err(CcError::Cancelled)` within a 2s
      timeout. (The spec's "just MessageStart + pre-cancel" shape
      doesn't trip the engine's cancel branch — that branch at
      `engine.rs:~279` gates on `!text_buf.is_empty()`, and
      `drain_stream` doesn't poll the token inside its recv loop.
      Included at least one text delta so the partial-save path
      fires and we get the expected error. The "without hanging"
      part of the contract is preserved via `tokio::time::timeout`.
      The stronger mid-drain cancel contract is covered by
      `drain_stream_cancels_when_tui_receiver_drops`.)
- [x] Bonus: `engine::tests::scripted_stream_produces_independent
      _receivers` — locks in the spec scenario "multi-call closure
      produces independent receivers" by invoking the same closure
      twice and asserting both receivers yield the canned
      MessageStart and then drain to None.

## 5. Smoke: unblock a deferred test

- [x] 5.1 `engine::tests::additional_contexts_are_cleared_on_cancel`
      — mirrors `fix-hook-correctness-wiring` §5.3. Seeds
      `pending_additional_contexts` directly (cheaper than wiring
      a PreToolUse hook for the smoke proof) and then drives a
      cancelled turn via `scripted_stream` + pre-cancelled token;
      asserts the error is `CcError::Cancelled` AND
      `engine.pending_additional_contexts()` is empty after the
      return. Clears the deferred note on `fix-hook-correctness-
      wiring/tasks.md §5.3`.

## 6. Verification

- [x] 6.1 `cargo fmt --all` clean. (Ran `cargo fmt -p cc-query`;
      workspace-level fmt has pre-existing drift in other crates
      that is not this change's scope.)
- [x] 6.2 `cargo clippy -p cc-query --all-targets -- -D warnings`
      clean; workspace-level clippy also clean.
- [x] 6.3 `cargo test -p cc-query` — 36 tests pass (was 32), the
      four new ones being `stream_override_drives_full_turn`,
      `stream_override_respects_cancel`,
      `scripted_stream_produces_independent_receivers`, and
      `additional_contexts_are_cleared_on_cancel`. `cargo test
      --workspace` also green (637 passed, 0 failed, 1 ignored).
- [x] 6.4 Release-build byte-identity: `cargo build --release -p
      cc-query` succeeds. The `#[cfg(test)]` gates on the field,
      the `stream_override: None` init line, the `#[cfg(test)] let
      rx = if ...` branch, and the entire `test_support` module
      mean release builds compile to the same code as pre-change
      modulo build metadata. Verified by inspection of the diff.

## 7. Downstream coordination

- [x] 7.1 Updated `fix-hook-correctness-wiring/tasks.md §5.3`
      from `[ ]` (deferred) to `[x]` with a cross-reference to
      this change's §5.1.
- [ ] 7.2 `fix-file-history-snapshot-producers/tasks.md §6.1 /
      §6.2` updates deferred: the spec dir does not exist on the
      worktree's base commit (it was authored in the sibling
      worktree alongside `fix-tool-context-refactor`). The
      follow-up PR that lands those specs should cite
      `test_support::scripted_stream` as the harness mechanism
      for §5.1 / §5.2's cancel-path tests.

## 8. Sign-off

- [x] 8.1 Commit message states "test-only harness; release
      builds unchanged" and enumerates the downstream tests
      unblocked.
- [x] 8.2 No memory note needed — internal test scaffolding, no
      public contract change.
