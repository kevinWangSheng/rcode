# Risk Inventory — Phase 4

Risk Score = Unknownness (U) × Blast Radius (B) × Reversibility (R). Max = 27.
HIGH = score ≥ 12.

---

## Risk 1: TUI — Ink/React → Ratatui

**Context:** The TS version uses React/Ink for the entire interactive terminal UI — streaming
output, permission dialogs, tool progress spinners, diffs, autocomplete, keybindings, themes.
Ratatui is the Rust equivalent but uses an immediate-mode rendering model, not a component tree.

| Dimension | Score | Reasoning |
|-----------|-------|-----------|
| Unknownness (U) | 3 | Ratatui is known, but mapping React component lifecycle, focus management, event bubbling, and real-time streaming updates to immediate-mode requires original design |
| Blast Radius (B) | 3 | TUI is used by nearly every crate — session display, tool output, permission dialogs, autocomplete. Wrong architecture ripples everywhere |
| Reversibility (R) | 3 | TUI framework choice baked into the rendering loop, event loop, and every screen component |

**Score: 3 × 3 × 3 = 27 — HIGH**

### Spike: Ratatui Streaming + Event Handling

**Spike Goal:**
Can Ratatui render streaming text (token-by-token) from an async Tokio channel while simultaneously
handling keyboard events (Ctrl+C abort, Enter submit) without visual tearing or event loss in an
80-column macOS terminal?

**Spike Prototype:**
A standalone binary (`spike-tui`) that:
- Opens a Ratatui TUI with an input box at the bottom and output panel above
- Simulates Claude streaming (50ms delay between tokens) via a Tokio channel
- Handles Ctrl+C to abort streaming mid-response
- Handles Enter to submit a new message while streaming is active
- Shows a spinner during streaming

**Acceptance Criteria:**
- [ ] Streaming updates render at ≥ 30fps without tearing on macOS Terminal.app (80 col)
- [ ] Ctrl+C during streaming stops stream within 100ms and preserves partial text
- [ ] Enter key while streaming is received and queued (not dropped)
- [ ] Memory usage stays flat over 100 simulated streaming turns
- [ ] Ratatui event loop and Tokio async runtime integrate without deadlock

**Scope Assessment:** Can one agent complete in a single session? **Yes** — standalone binary.

---

## Risk 2: Streaming API Handling — SSE Parsing + Backpressure in Tokio

**Context:** Claude API responses are Server-Sent Events. The TS version uses the Anthropic SDK
for SSE parsing, streaming thinking blocks, tool_use blocks with partial JSON, and abort signals.
Rust needs a custom or crate-based SSE consumer integrated with Tokio async.

| Dimension | Score | Reasoning |
|-----------|-------|-----------|
| Unknownness (U) | 2 | `reqwest` + `eventsource-stream` crates exist; abort via `CancellationToken` is known Tokio pattern. Edge cases: partial tool JSON, interleaved thinking blocks |
| Blast Radius (B) | 2 | Affects `cc-api` and `cc-session` directly; other crates consume the stream via trait |
| Reversibility (R) | 2 | Stream interface can be abstracted behind a trait; swapping implementation is medium effort |

**Score: 2 × 2 × 2 = 8 — MEDIUM**

**Mitigation:** Define a `Stream<Item = StreamEvent>` trait early. Implement SSE parsing behind it.
Test against real API with partial JSON tool_use inputs before integrating into `cc-session`.

---

## Risk 3: Plugin System — JS Plugins via stdio Bridge

**Context:** The TS version supports plugins (skills, commands) written in JS/TS loaded in-process.
A Rust version cannot run JS natively — would need a stdio bridge (subprocess) or skip JS plugins.
Given 80% parity goal and macOS-first, JS plugins are lower priority.

| Dimension | Score | Reasoning |
|-----------|-------|-----------|
| Unknownness (U) | 3 | No clear path: embedding V8 (Deno/Node) is complex; stdio bridge is simpler but changes plugin API |
| Blast Radius (B) | 1 | Isolated to `cc-plugins` crate; core functionality unaffected |
| Reversibility (R) | 1 | Plugin system is behind an interface; can add later |

**Score: 3 × 1 × 1 = 3 — LOW**

**Decision:** Defer JS plugin support. Rust-native skills (markdown-based, same format) work
natively. JS plugins can be added later via stdio subprocess bridge.

---

## Risk 4: MCP Transport — stdio + HTTP SSE in Rust

**Context:** MCP clients must support 4 transport types: stdio, HTTP SSE, Streaming HTTP, WebSocket.
Rust has `rmcp` crate and `mcp-rs` ecosystem. The protocol is JSON-RPC 2.0 over these transports.

| Dimension | Score | Reasoning |
|-----------|-------|-----------|
| Unknownness (U) | 2 | `rmcp` crate covers stdio + SSE. WebSocket with mTLS is less documented |
| Blast Radius (B) | 2 | Affects `cc-mcp` and `cc-tools` (MCP-derived tools) |
| Reversibility (R) | 1 | Behind a transport trait; easy to swap |

**Score: 2 × 2 × 1 = 4 — LOW**

**Mitigation:** Use `rmcp` crate for stdio + SSE first. Validate against real MCP server (e.g.,
filesystem MCP server) in Phase 1 spike. Add WebSocket transport later.

---

## Risk 5: OAuth Flow — Browser Launch + Local HTTP Redirect

**Context:** Claude.ai login uses OAuth2 with browser launch and localhost redirect capture.
Rust has `oauth2` crate and `tiny_http` or `axum` for the local server. Standard pattern but
requires correct PKCE, state param, and redirect URI handling.

| Dimension | Score | Reasoning |
|-----------|-------|-----------|
| Unknownness (U) | 2 | OAuth2 + PKCE is well-documented; `oauth2` crate handles it. Browser launch via `open` crate. |
| Blast Radius (B) | 1 | Isolated to `cc-auth` crate |
| Reversibility (R) | 1 | Auth is behind a trait; implementation swap is easy |

**Score: 2 × 1 × 1 = 2 — LOW**

---

## Risk 6: Platform Keychain — macOS/Windows/Linux Secure Storage

**Context:** API keys must be stored securely. macOS uses Keychain, Linux uses secret-service/D-Bus,
Windows uses Credential Manager. The `keyring` crate unifies these.

| Dimension | Score | Reasoning |
|-----------|-------|-----------|
| Unknownness (U) | 1 | `keyring` crate (v2/v3) handles all three platforms with a unified API |
| Blast Radius (B) | 1 | Isolated to `cc-auth` |
| Reversibility (R) | 1 | Behind a trait interface |

**Score: 1 × 1 × 1 = 1 — LOW**

---

## Risk 7: Terminal Input Handling — crossterm Edge Cases

**Context:** `crossterm` handles keyboard input in Rust TUIs. Edge cases include: vim-mode input
(escape sequences), chord keybindings (Ctrl+Shift), paste detection, resize events, and
interaction with Ratatui's event loop.

| Dimension | Score | Reasoning |
|-----------|-------|-----------|
| Unknownness (U) | 2 | crossterm is mature; most inputs work. Chord keybindings (Ctrl+Shift+P) and terminal-specific escape sequences vary by terminal emulator |
| Blast Radius (B) | 2 | Affects `cc-tui` and `cc-commands` (keybinding dispatch) |
| Reversibility (R) | 2 | Keybinding layer can be refactored but touches event dispatch throughout TUI |

**Score: 2 × 2 × 2 = 8 — MEDIUM**

**Mitigation:** Build a keybinding abstraction layer early. Test chord bindings on macOS Terminal,
iTerm2, and VS Code terminal before finalizing the input model.

---

## Risk 8: Large Output Rendering — Diff Display + Syntax Highlight Performance

**Context:** The TS version renders large diffs, syntax-highlighted code, and paginated tool output.
Ratatui with `syntect` or `bat` for highlighting needs to handle 10,000+ line outputs without
blocking the event loop.

| Dimension | Score | Reasoning |
|-----------|-------|-----------|
| Unknownness (U) | 2 | `syntect` is the standard; Ratatui scrollable widgets exist. Large output streaming needs incremental rendering design |
| Blast Radius (B) | 2 | Affects `cc-tui` rendering pipeline |
| Reversibility (R) | 2 | Rendering pipeline can be refactored, but changes touch all display components |

**Score: 2 × 2 × 2 = 8 — MEDIUM**

**Mitigation:** Implement virtual scrolling (render only visible lines). Test with 10K line outputs.
Use `bat`'s `PrettyPrinter` for highlighting rather than building from scratch.

---

## Risk 9: Session Compaction — Context-Window-Aware Truncation Logic

**Context:** Auto-compaction calls the Claude API with the current conversation to generate a
summary. Requires exact token counting (tiktoken equivalent in Rust), threshold calculation,
and message reconstruction. Phase 3 identified this as MUST REPLICATE EXACTLY.

| Dimension | Score | Reasoning |
|-----------|-------|-----------|
| Unknownness (U) | 2 | Token counting in Rust: `tiktoken-rs` crate exists. Threshold logic is documented in Phase 3. API call for summary is standard. |
| Blast Radius (B) | 2 | Affects `cc-session` and `cc-api`; compaction is part of the core query loop |
| Reversibility (R) | 2 | Compaction logic is mostly self-contained but integrates into the main query flow |

**Score: 2 × 2 × 2 = 8 — MEDIUM**

**Mitigation:** Implement `tiktoken-rs` token counting early. Validate threshold formula against
TS version with identical conversation histories. Test compaction round-trip (compact → resume).

---

## Risk 10: Multi-Agent / Task Subsystem — Heterogeneous Concurrency Model

> **Rewritten 2026-04-08.** The previous version of this risk claimed "The TS
> version uses Unix Domain Sockets for inter-agent communication." **This was
> factually wrong.** A full-source search (`UnixListener`, `UnixStream`,
> `.sock`, `socketPath`, `net.createServer.*unix`) found **no UDS IPC anywhere
> in the TS codebase**. The real architecture is documented in Decision 6 of
> `.claude/plan/phase2-entry.md`. This risk entry has been rewritten to reflect
> the actual TS design and the Phase 2 in-scope subset.

**Context:** The TS multi-agent / task subsystem is multi-modal, not UDS-based:

- `local_bash` — background subprocess (pipe)
- `local_agent` — **in-process** sub-agent, independent query loop, same address space
- `in_process_teammate` — **in-process** multi-teammate via shared `AppState`
- `remote_agent` — **HTTP polling** of Anthropic-side remote session API

Plus 3 feature-flagged experimental task types (`local_workflow`, `monitor_mcp`, `dream`) — **deferred per Decision 6**.

The real risk is not "designing an IPC protocol" (there isn't one) but **mapping a JS single-threaded event-loop concurrency model with shared `AppState` onto Rust's ownership + `Send`/`Sync` rules.** In TS, the main session, teammates, and local agents all read and mutate the same `AppState` object; in Rust this has to become some combination of `Arc<RwLock<...>>` / message-passing / actor model — and the wrong choice propagates through `cc-query` and `cc-tui`.

| Dimension | Score | Reasoning |
|-----------|-------|-----------|
| Unknownness (U) | 3 | No clear Rust-idiomatic mapping of "many in-process actors sharing mutable state." Choice between `Arc<Mutex<AppState>>`, tokio channels, or an actor framework (e.g. `ractor`) is non-obvious and reversibility is low once chosen |
| Blast Radius (B) | 3 | Touches `cc-query` (task spawn + abort), `cc-session` (state persistence), `cc-tui` (teammate display), and whatever swarm crate Decision 3 adopts |
| Reversibility (R) | 2 | Concurrency model bakes into traits and `async fn` signatures; changing mid-project is expensive but not impossible behind a state-access trait |

**Score: 3 × 3 × 2 = 18 — HIGH**

**Mitigations (for Phase 2 detailed design):**
1. Pick the concurrency model in Decision 1 (async runtime) + a follow-up sub-decision for state-access pattern, **before** writing A1 contracts.
2. Add a small spike (*not* the old M0–M4 spikes — a new targeted one) for `in_process_teammate`: 2 teammates sharing a `RwLock<AppState>`, one reads while the other writes, measuring contention.
3. Make `AppState` access go through a trait from day one, so the underlying concurrency primitive can be swapped.

**Remote-agent-specific risks:** HTTP polling against `api.anthropic.com` (or the relevant remote session API) has its own sub-risks:
- Rate limits on long polling
- Partial event delivery / cursor recovery on network failure
- Authentication reuse (the remote agent needs the same OAuth/API key auth as the main session)

These sub-risks are inherited from Risk 2 (Streaming API) and Risk 5 (OAuth) — no new mitigation needed beyond what those entries already specify.

---

## Risk 11: Tokenizer Parity — Token Counting for Auto-Compact

> Added 2026-04-09 per Decision 7 (phase2-entry.md).

**Context:** Phase 1 originally classified this as a blocking concern: "Anthropic
doesn't publish a tokenizer, and `tiktoken-rs` is for OpenAI models." However,
source analysis (Decision 7) revealed the TS version **does not use an exact
tokenizer** for auto-compact threshold checks. It uses `usage.input_tokens` from
the last API response + a rough character-length heuristic (`content.length / 4`)
for the delta. This approach is trivially replicable in Rust.

| Dimension | Score | Reasoning |
|-----------|-------|-----------|
| Unknownness (U) | 1 | TS approach fully analyzed; Rust implementation is straightforward arithmetic |
| Blast Radius (B) | 1 | Contained to `cc-query` compaction check + `cc-api` usage extraction |
| Reversibility (R) | 1 | Estimation constants can be tuned without architectural change |

**Score: 1 × 1 × 1 = 1 — LOW**

**Mitigations:** None needed. Implement the same hybrid approach as TS:
store `usage` from API responses, add `content.len() / 4` delta estimation.
The 13,000-token buffer absorbs estimation error.

---

## Summary Table

| # | Risk Area | U | B | R | Score | Level |
|---|-----------|---|---|---|-------|-------|
| 1 | TUI (Ink/React → Ratatui) | 3 | 3 | 3 | **27** | **HIGH** |
| 2 | Streaming API (SSE + Tokio) | 2 | 2 | 2 | 8 | MEDIUM |
| 3 | Plugin System (JS bridge) | 3 | 1 | 1 | 3 | LOW |
| 4 | MCP Transport | 2 | 2 | 1 | 4 | LOW |
| 5 | OAuth Flow | 2 | 1 | 1 | 2 | LOW |
| 6 | Platform Keychain | 1 | 1 | 1 | 1 | LOW |
| 7 | Terminal Input (crossterm) | 2 | 2 | 2 | 8 | MEDIUM |
| 8 | Large Output Rendering | 2 | 2 | 2 | 8 | MEDIUM |
| 9 | Session Compaction | 2 | 2 | 2 | 8 | MEDIUM |
| 10 | Multi-Agent / Task Subsystem (rewritten 2026-04-08) | 3 | 3 | 2 | **18** | **HIGH** |
| 11 | Tokenizer Parity (added 2026-04-09) | 1 | 1 | 1 | 1 | LOW |

**HIGH risks requiring Spike: Risk 1 (TUI), Risk 10 (Multi-Agent — new targeted spike needed)**
**MEDIUM risks requiring mitigation: Risks 2, 7, 8, 9**
**LOW risks (no action): Risks 3, 4, 5, 6, 11**

> **Note (2026-04-08):** Risk 10 was upgraded from LOW (score 2) to HIGH (score 18) after correcting the factual premise. See Risk 10 entry for the rewrite rationale.
> **Note (2026-04-09):** Risk 11 added per Decision 7. Scored LOW because the original concern was based on a false premise — the TS version uses a rough heuristic, not an exact tokenizer.
