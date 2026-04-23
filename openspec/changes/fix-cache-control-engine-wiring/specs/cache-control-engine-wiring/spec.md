# Spec — Engine-side cache-breakpoint tagging

## MODIFIED Requirements

### Requirement: Engine applies trailing-block breakpoint on every API request

The `cc-query` engine SHALL splat `cache_control = { "type":
"ephemeral" }` onto the trailing content block of the trailing
`MessageParam` in the `messages` vector before constructing the
`CreateMessageRequest` on each loop iteration of `run_turn`.

The engine SHALL NOT apply breakpoints to earlier messages. The
engine SHALL NOT apply a breakpoint to a trailing block whose
variant is `Thinking`, `RedactedThinking`, or `Unknown` (that case is
enforced by the `ContentBlock::with_cache_control` no-op paths in
`cc-core`).

The engine SHALL NOT apply a breakpoint when the trailing message's
content is `MessageContent::Text` rather than `MessageContent::Blocks`
— this matches the TS reference (which stores user text as a single
block array already; the Rust mismatch is a separate, non-blocking
gap flagged as open question #1 in the proposal).

#### Scenario: Two-message trailing-block tagging

- **Given** `messages = [User(Blocks[Text("a")]), Assistant(Blocks[Text("b"), ToolUse{…}])]`
- **When** `tag_last_block_for_caching(&mut messages)` runs
- **Then** `messages[0].content.blocks[0].cache_control == None`
- **And** `messages[1].content.blocks[0].cache_control == None`
- **And** `messages[1].content.blocks[1].cache_control == Some({type: "ephemeral"})`

#### Scenario: Thinking trailing block is skipped

- **Given** `messages = [Assistant(Blocks[Text("x"), Thinking("…")])]`
- **When** the helper runs
- **Then** the `Thinking` block's `cache_control` is still `None`
- **And** the `Text("x")` block's `cache_control` is still `None`
  (only the true trailing block is considered, and the helper does
  not "fall back" to earlier blocks when the trailing one is
  excluded)

#### Scenario: String-content messages are left alone

- **Given** `messages = [User(Text("hi"))]`
- **When** the helper runs
- **Then** the message is unchanged (no breakpoint anywhere; TS parity)

#### Scenario: Idempotence

- **Given** a message vector where the trailing block already carries
  a breakpoint from a prior call
- **When** the helper runs a second time
- **Then** the trailing block still carries exactly one ephemeral
  breakpoint (overwrite semantics, not duplicate)

#### Scenario: Request body carries the breakpoint on the wire

- **Given** a `QueryEngine` executing `run_turn` with a non-empty
  message vector
- **When** the engine constructs `CreateMessageRequest` and serialises
  it via `serde_json`
- **Then** the resulting JSON body contains `"cache_control":
  {"type":"ephemeral"}` on the trailing block of the trailing message
- **And** no earlier block in any message carries a `cache_control`
  field in the serialised body
