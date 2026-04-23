# Proposal — Wire content-block cache_control into the query engine

## Why

`fix-content-block-cache-control` (commit `790a749`, merged 2026-04-23)
added the `cache_control` field to `ContentBlock::{Text,ToolUse,
ToolResult,Image}` and shipped a `ContentBlock::with_cache_control(…)`
helper. All field-level serde tests pass. **But the helper has zero
production call sites.** The 2026-04-23 QA pass on the four Batch-A/B/C
changes found:

```
$ rg -n "with_cache_control" rust/crates/cc-query rust/cc
# — no results (test-only references in cc-api and cc-core) —
```

`QueryEngine::run_turn` (`rust/crates/cc-query/src/engine.rs:159-210`)
builds every `CreateMessageRequest` with `messages.clone()` and never
tags any block before handing it to `stream_message`. This means the
original parity gap **P0 #1** (prompt-caching dead on agentic tool
loops) is **still open** end-to-end — the type now *can* carry a
breakpoint, but the engine never puts one there.

The TS reference applies the breakpoint inside its `queryModel` wrapper
via `addCacheBreakpoints(messages)` (`src/services/api/claude.ts`,
around the `claude.ts:1701-1705` call site) — specifically, it locates
the **last content block of the last message** in the request and
splats `cache_control: { type: "ephemeral" }` onto it, skipping the
Thinking / RedactedThinking variants. Without a Rust equivalent, the
rewrite always sends un-tagged request bodies and `usage.cache_read_
input_tokens` stays at zero on every turn after the first.

## Goal

Re-establish TS parity for turn-level prompt caching by attaching an
ephemeral breakpoint to the trailing content block of the trailing
message on every request the engine sends.

Not in scope:

- System-block cache tiering (already handled by callers per the
  contract in `engine.rs:47-60`).
- Tool-definition cache_control (unchanged — the field already exists
  on `ToolDefinition`; callers populate it).
- Multi-breakpoint placement (TS supports up to 4 breakpoints; the
  common case and the only one we need for 80% parity is the trailing
  block).

## What changes

### 1. New helper on the request-builder path

Add a pure helper that takes `&mut Vec<MessageParam>` and tags the
trailing block of the trailing message. Place it next to
`CreateMessageRequest` or in a new small `cache_breakpoint.rs` inside
`cc-query` (it depends on both `MessageParam` / `ContentBlock` and on
the specific "trailing block of trailing message" policy, which is a
cc-query concern, not a cc-core one).

```rust
// rust/crates/cc-query/src/cache_breakpoint.rs (new file)
use cc_core::{CacheControl, ContentBlock, MessageContent, MessageParam};

/// Splat `cache_control = ephemeral` onto the trailing block of the
/// trailing message, matching TS `addCacheBreakpoints`.
///
/// No-ops when `messages` is empty. `ContentBlock::with_cache_control`
/// already no-ops on Thinking / RedactedThinking / Unknown variants,
/// so callers never need to inspect the block type.
///
/// Idempotent: calling it twice on the same vector leaves the same
/// block tagged exactly once (the second call overwrites with the same
/// value).
pub fn tag_last_block_for_caching(messages: &mut Vec<MessageParam>) {
    let Some(last_msg) = messages.last_mut() else { return };
    let MessageContent::Blocks(blocks) = &mut last_msg.content else {
        // String-content messages can't carry a breakpoint — TS treats
        // these the same way (the splat path runs on block arrays).
        return;
    };
    let Some(last_block) = blocks.pop() else { return };
    blocks.push(last_block.with_cache_control(CacheControl::ephemeral_unscoped()));
}
```

Note: `MessageParam::content` is `MessageContent` (can be `String` or
`Blocks`). For the `String` case, the tag doesn't reach the wire — TS
has the same limitation, so this is TS parity, not a regression. If
this becomes a gap in practice, switch `user_text.into()` in
`run_turn` line 170 to emit a `MessageContent::Blocks(vec![ContentBlock::
text(…)])` so the block is taggable; see Open Questions.

### 2. Call site in `QueryEngine::run_turn`

Insert exactly one call between request construction and send, inside
the `loop { … }` body:

```rust
// rust/crates/cc-query/src/engine.rs around line 189-197, BEFORE the
// `stream_message` call at line 210:

tag_last_block_for_caching(messages);

let mut req = CreateMessageRequest::new(&self.options.model, messages.clone())
    .with_max_tokens(self.options.max_tokens);
```

Placement rationale: tagging after every loop-iteration push (user msg
at line 172; assistant msg at the reconstruction point after drain;
tool-result msg; error tool-results) means whichever message is
logically last at request time receives the breakpoint. The earlier
messages from previous iterations retain whatever `cache_control` they
already had (typically `None`, which is correct — only the trailing
message should carry the breakpoint so the API caches everything up
to but not including the new input).

### 3. Regression tests

Add three tests in `cc-query`:

- **Unit** `cache_breakpoint::tests::tag_empty_vec_is_noop`
- **Unit** `cache_breakpoint::tests::tag_trailing_block_of_trailing_message`
  — construct a two-message vector; assert only the last block of the
  last message carries `cache_control`; assert earlier blocks do not.
- **Unit** `cache_breakpoint::tests::tag_is_idempotent`
- **Unit** `cache_breakpoint::tests::tag_skips_thinking_trailing_block`
  — trailing block is `Thinking` → no-op (matches TS exclusion).
- **Integration** in `engine.rs::tests::request_body_carries_ephemeral
  _on_last_block` — build a minimal `QueryEngine` with a stub `ApiClient`
  that captures the `CreateMessageRequest`, run one turn, assert the
  serialised body contains `"cache_control":{"type":"ephemeral"}` on
  the trailing block only. Use the existing `StubApi` pattern if present
  or introduce one inline.

### 4. Verification plan

- `cargo test -p cc-query` — new tests green.
- `cargo test -p cc-core -p cc-api` — unchanged (regression guard).
- Manual live-API smoke: run a short 3-turn interactive session, watch
  `ApiUsage::cache_read_input_tokens` via debug logs; expect `> 0` on
  turn 2+. (This is an observational sanity check; leave it out of CI.)

## Impact

- **Affected specs**: `content-block-cache-control` (extended from
  "the field MAY be present" to "the engine MUST tag the trailing
  block on every request").
- **Affected crates**: `cc-query` (new module + one call site + tests).
  No changes to `cc-api`, `cc-core`, or `cc-tui`.
- **Wire compatibility**: additive — requests that previously carried
  no `cache_control` now carry one on the trailing block. The API
  accepts this already (ephemeral breakpoints are the documented
  prompt-cache opt-in).
- **Cost impact**: expected to *reduce* API cost on multi-turn
  sessions by ≥30% once hits land on turn 2 (rough TS-reference figure;
  exact ratio depends on input shape).

## Open questions

1. Should string-content user messages (`MessageParam::user("hi")` →
   `MessageContent::Text`) also be made taggable by promoting them to
   single-block vectors inside `user_text.into()`? TS stores user text
   as a single-element block array already, so this would close a
   small parity gap. Flag for follow-up; not required to fix the
   bigger issue.
2. Do we need a kill-switch config (`CLAUDE_DISABLE_PROMPT_CACHE=1`)
   to opt out? TS exposes `enablePromptCaching` in settings. The
   80%-parity bar says "not required" but it's a ~5-LOC add if we
   want symmetry.

Both questions are non-blocking; ship without them.
