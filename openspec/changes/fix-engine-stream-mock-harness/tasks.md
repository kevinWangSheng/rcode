## 1. Field + routing in QueryEngine

- [ ] 1.1 In `rust/crates/cc-query/src/engine.rs`, add
      `#[cfg(test)] stream_override: Option<test_support::
      StreamOverrideFn>` to the `QueryEngine` struct (around
      `engine.rs:61-74`).
- [ ] 1.2 Initialise to `None` in `QueryEngine::new` (search for
      the existing initialiser; add the field init under a
      `#[cfg(test)]` block).
- [ ] 1.3 At the single `self.api.stream_message(...)` call site
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

- [ ] 2.1 Add `#[cfg(test)] pub(crate) mod test_support` at the
      bottom of `engine.rs` (or in a sibling `engine/test_support.rs`
      if the file is getting long — preference: same file, to keep
      test-only code adjacent to the private fields it touches).
- [ ] 2.2 Define:
      ```
      pub(crate) type StreamOverrideFn = std::sync::Arc<
          dyn Fn(&CreateMessageRequest, &CancellationToken)
                 -> tokio::sync::mpsc::Receiver<CcResult<StreamEvent>>
              + Send + Sync,
      >;
      ```
- [ ] 2.3 Implement `QueryEngine::with_stream_override<F>(self,
      f: F) -> Self` that stores the closure in the field. Mark
      `pub(crate)` — nothing outside `cc-query` should need it.
- [ ] 2.4 Implement the `scripted_stream(events:
      Vec<CcResult<StreamEvent>>) -> impl Fn(...) -> ...`
      convenience helper. Handles the common "just replay N events"
      case. Spawns a tokio task per call that pushes the events
      down the channel; degrades `Err(_)` to
      `Err(CcError::Other("scripted-stream error"))` (see §3).

## 3. CcError handling

- [ ] 3.1 Check whether `cc_core::CcError` can derive `Clone`
      without touching other code. If yes, add
      `#[derive(..., Clone)]` and rewrite `scripted_stream` to
      pass errors through faithfully. If no (e.g. wraps a
      non-Clone dependency error), keep the
      `CcError::Other("scripted-stream error")` degrade.
- [ ] 3.2 Document the chosen path in the commit message so the
      next test author understands the error-fidelity tradeoff.

## 4. Self-regression tests

- [ ] 4.1 `engine::tests::stream_override_drives_full_turn` —
      build an engine with a scripted stream that yields a
      `MessageStart` + one text delta + `MessageStop`; call
      `run_turn("hi", …)`; assert the returned final text equals
      the delta content.
- [ ] 4.2 `engine::tests::stream_override_respects_cancel` —
      scripted stream that yields nothing but a `MessageStart`;
      cancel the token before `run_turn`; assert the call returns
      `Err(CcError::Cancelled)` without hanging. (If this is
      tricky because the cancel path needs to be exercised by the
      drain_stream side, accept a `await` on a pre-cancelled token
      yielding the cancel error promptly.)

## 5. Smoke: unblock a deferred test

Pick ONE previously-deferred test from the roadmap and write it
here as a "the harness works" proof point. Low-risk choice:

- [ ] 5.1 `engine::tests::additional_contexts_are_cleared_on_cancel`
      — mirrors `fix-hook-correctness-wiring` §5.3. Populate
      `pending_additional_contexts` via a PreToolUse hook that
      emits a context; wire a stream override that yields one
      MessageStart + a text delta + a Cancelled error (or cancel
      the token right after the delta); assert the error surfaces
      AND `engine.pending_additional_contexts()` is empty after
      the `run_turn` returns Err.

      If this test clashes with state isolation in other tests,
      feel free to move the unlock proof to
      `fix-file-history-snapshot-producers` instead and leave
      this task as `[ ]` with a pointer.

## 6. Verification

- [ ] 6.1 `cargo fmt --all` clean.
- [ ] 6.2 `cargo clippy -p cc-query --all-targets -- -D warnings`
      clean.
- [ ] 6.3 `cargo test -p cc-query` — all existing tests pass,
      plus the 3 new ones from §4 and §5.
- [ ] 6.4 **Release-build byte-identity**: `cargo build --release
      -p cc-query` before and after the change should produce
      the same library bytes (modulo timestamp / build-id). The
      `#[cfg(test)]` gates make this true if the field and the
      `let rx = …` branch are both correctly gated. Spot-check
      by reading the diff.

## 7. Downstream coordination

- [ ] 7.1 Update `fix-hook-correctness-wiring/tasks.md` §5.3 from
      `[ ]` with the deferred note to `[x]` (or leave `[ ]` with a
      cross-reference to this change's §5.1 proof-of-concept) —
      decision made in the commit that lands this change.
- [ ] 7.2 Update `fix-file-history-snapshot-producers/tasks.md`
      §6.1 and §6.2 notes to reference the harness as the
      mechanism for driving the cancel flow.

## 8. Sign-off

- [ ] 8.1 Commit message states: "test-only harness; release builds
      unchanged". List the three downstream tests this unblocks.
- [ ] 8.2 No memory note needed — this is internal test scaffolding
      that doesn't change any public contract.
