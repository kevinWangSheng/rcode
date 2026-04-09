# Phase 2 Entry — Open Decisions Workspace

**Status:** COMPLETE (2026-04-09) — All entry gate items resolved. Phase 2 design can begin.
**Created:** 2026-04-08
**Purpose:** Resolve the 7 Open Decisions left at the end of Phase 1 so that
Phase 2 (detailed design) can begin on solid ground.

---

## Scope

This file is the working space for closing the Phase 2 entry gate. It is **not**
a detailed design document — it only captures the foundational decisions that
must be made *before* detailed design can start.

**Inputs:**
- `RUST_REWRITE_PLAN.md` §1–7 (Phase 1 planning output)
- `RUST_REWRITE_PLAN.md` §2 "Open Decisions — Phase 2 Entry Gate"
- `.claude/plan/state.md` (spike inventory — reference only, not binding)
- `rust/` spike code (reference only, not binding)

**Output:** Each decision below moved from `UNRESOLVED` to `DECIDED`, with
rationale and implications recorded. When all 7 are `DECIDED` and the Phase 1
gap patches (A1–A3) are filed, Phase 2 detailed design can begin in a new
document (proposed: `.claude/plan/phase2-design.md`).

**Ground rule:** The existence of spike code in `rust/` is **evidence of
viability**, not a decision. A decision to adopt a spike approach must still be
made explicitly and recorded here with rationale.

---

## Resolution Order (agreed 2026-04-08)

Decisions and gap patches are interleaved in this order, because later items
depend on earlier ones:

1. ~~**Decision 5** (MCP transports + mTLS scope)~~ — **DECIDED 2026-04-08**
2. ~~**Decision 6** (Multi-agent / task subsystem scope)~~ — **DECIDED 2026-04-08**
3. ~~**Phase 1 gap patch A3** — project discovery / `.claude/` rules / CLAUDE.md loading~~ — **DONE 2026-04-09**
4. ~~**Phase 1 gap patch A2** — hooks lifecycle (6+ trigger points, ordering, blocking semantics)~~ — **DONE 2026-04-09**
5. ~~**Decision 7** — Tokenizer parity strategy~~ — **DECIDED 2026-04-09**
6. ~~**Phase 1 gap patch A1** — tasks/agent behavior contracts for the 4 stable task types~~ — **DONE 2026-04-09**
7. ~~**Decisions 1–4** — async runtime, TUI library, crate layout, 80% feature cut~~ — **ALL DECIDED 2026-04-09**
8. ~~**Spike audit**~~ — **DONE 2026-04-09** (6 REUSE, 6 REWRITE, 8 DISCARD)
9. ~~**Open `phase2-design.md`**~~ — **DONE 2026-04-09**

Decisions 1–4 sit last because they should reflect the information surfaced by
the gap patches and Decisions 5–7 — not the other way around.

---

## Decision 1 — Async Runtime

**Status:** DECIDED (2026-04-09)

**Question:** Which async runtime will the Rust rewrite use?

**Spike finding:** Tokio was used throughout the M0–M4 exploration. No
blockers were encountered.

**Options to consider:**
- Tokio (spike default; mature ecosystem; matches `rmcp`, `reqwest`, `ratatui` assumptions)
- async-std (smaller ecosystem footprint; unclear fit with MCP/HTTP stack)
- smol (minimal; would require more glue)
- Custom / no runtime (not realistic for streaming + MCP + HTTP + TUI)

**Decision:** Tokio (multi-threaded runtime, `#[tokio::main]`).

**Rationale:**

1. **Ecosystem lock-in is real and acceptable.** `reqwest` (HTTP client for API + MCP), `rmcp` (MCP SDK), `tokio-tungstenite` (future WebSocket), and `ratatui`'s async event loop all assume Tokio. Choosing anything else means fighting the ecosystem.
2. **Spike validated.** M0–M4 used Tokio with no blockers across streaming, tool execution, MCP, and TUI.
3. **Concurrency model fits.** The task subsystem (A1 contracts) requires: subprocess management (`tokio::process`), cancellation tokens (`CancellationToken` from `tokio-util`), background tasks (`tokio::spawn`), shared state (`Arc<RwLock<T>>`), and timer-based polling (`tokio::time`). All first-class in Tokio.
4. **No compelling alternative.** async-std's ecosystem is smaller; smol would require glue for every dependency. The learning goal is better served by mastering the dominant runtime.

**Implications for detailed design:**
- **Task model:** `tokio::spawn` for background tasks; `JoinHandle` for tracking; `CancellationToken` for graceful abort (maps to TS's AbortController)
- **Cancellation:** Two-level abort pattern from A1 (teammate turn vs full kill) maps to nested `CancellationToken` trees
- **Blocking-IO policy:** Filesystem operations via `tokio::fs` where async matters (large reads); `std::fs` acceptable for small config reads at startup. `tokio::task::spawn_blocking` for CPU-heavy work.
- **Subprocess:** `tokio::process::Command` for bash tool and hook execution
- **Channel pattern:** `tokio::sync::mpsc` for mailbox system (in_process_teammate), `tokio::sync::watch` for state broadcasts

---

## Decision 2 — TUI Library

**Status:** DECIDED (2026-04-09)

**Question:** Which TUI library will the interactive `claude` terminal use?

**Spike finding:** Ratatui + crossterm was used in the M0 spike and passed
all 5 acceptance criteria (streaming ≥30fps, Ctrl+C <100ms, queued input, flat
memory over 100 turns, no deadlock). Evidence: `rust/spikes/tui/tests/headless.rs`.

**Options to consider:**
- Ratatui (spike default; validated by M0)
- Cursive (higher-level, event-driven; different mental model)
- Roll-our-own on crossterm (maximum control, maximum work)

**Decision:** Ratatui + crossterm.

**Rationale:**

1. **Spike validated all 5 ACs.** Streaming ≥30fps, Ctrl+C <100ms, queued input, flat memory, no deadlock — all confirmed in `rust/spikes/tui/tests/headless.rs`.
2. **Ratatui is the de facto standard.** Most actively maintained Rust TUI library (~15k GitHub stars), extensive widget ecosystem, strong Tokio integration.
3. **Immediate-mode rendering** matches the TS version's React/Ink model better than Cursive's retained-mode. Each frame re-renders from state — no widget lifecycle management.
4. **crossterm** provides cross-platform terminal abstraction (macOS primary, Linux later).
5. **Roll-our-own rejected:** Maximum control but massive effort; the spike proved Ratatui handles our needs without fighting the framework.

**Implications for detailed design:**
- **Component model:** Stateless render functions that take `&AppState` and produce `Frame` output. No widget instances to manage.
- **Rendering loop:** `tokio::select!` between terminal events (crossterm), engine events (API stream), and tick timer (~30fps). Re-render on any event.
- **Event routing:** crossterm `Event` → `AppAction` enum → state update → re-render. Permission dialogs, slash commands, input editing all expressed as state transitions.
- **Testing:** Headless backend (`TestBackend`) for unit tests; no terminal required. Port spike's 10 headless tests to production crate.
- **The abandoned M3 `cc-tui` spike code will be redesigned** in Phase 2 — it was structurally sound but incomplete. Phase 2 design should define the component tree and state model before rewriting.

---

## Decision 3 — Crate Workspace Layout and Naming

**Status:** DECIDED (2026-04-09)

**Question:** What crates exist, what does each own, and what are the
dependency rules between them?

**Spike finding:** The spike used a 20-crate split under `rust/crates/`:
`cc-core, cc-config, cc-analytics, cc-auth, cc-api, cc-permissions, cc-tools,
cc-hooks, cc-git, cc-mcp, cc-memory, cc-session, cc-query, cc-bridge, cc-skills,
cc-plugins, cc-commands, cc-tui, cc-tasks, cc-agent` + `rust/cc/` binary. This
was a convenient layout for parallel spike work, **not** a decision about
component boundaries.

**Questions this decision must answer:**
- How many crates? (fewer = faster builds, more = tighter boundaries)
- Which crates are library-public vs. internal?
- What is the layering rule? (who can depend on whom)
- Naming convention? (`cc-*` prefix, or something else)
- Does `cc-tasks` / `cc-agent` survive, or merge into others?

**Input constraints registered so far:**
- **From Decision 5 (2026-04-08):** `cc-api` and `cc-mcp` share a mTLS-aware HTTP
  client construction layer. Consider extracting a small `cc-http` crate (or
  similar) rather than duplicating the construction code.
- **From Decision 6 (2026-04-08):** The in-scope task/agent set
  (`local_agent` + `in_process_teammate` + `remote_agent` + `local_bash` +
  swarm InProcessBackend) pulls in ~3–5K LOC of TS source equivalents. The
  spike layout has stub `cc-tasks` and `cc-agent` crates; Decision 3 must
  consider merging these into a single `cc-swarm` (or `cc-agents`) crate, or
  splitting differently. The 3 deferred experimental tasks should not carry
  crate-level cost.

**Decision:** Consolidate from 20 spike crates to **14 library crates + 1 binary**, with layering rules.

**Revised crate layout:**

```
Layer 0:  cc-core           (types, traits, error)
Layer 1:  cc-config          (settings load/merge, project discovery)
          cc-http            (NEW: shared mTLS-aware HTTP client builder)
Layer 2:  cc-auth            (OAuth, Keychain, API key)
          cc-permissions     (allow/deny/ask rules)
          cc-git             (git root, worktree, ignore)
          cc-memory          (CLAUDE.md loading, walk-up, rules)
Layer 3:  cc-api             (Anthropic streaming client, uses cc-http)
          cc-tools           (8 built-in tools)
          cc-hooks           (hook execution engine, 27 events)
Layer 4:  cc-mcp             (stdio + sse + http transports, uses cc-http)
          cc-session         (JSONL transcript, resume)
Layer 5:  cc-agents          (MERGED: cc-tasks + cc-agent + swarm InProcess)
          cc-query           (tool loop, auto-compact, token tracking)
Layer 6:  cc-tui             (Ratatui interactive TUI + slash commands)
          cc-bridge          (SDK --print path)
---
Binary:   claude-cli         (CLI entry point)
```

**Changes from spike layout:**
- **MERGED** `cc-tasks` + `cc-agent` → `cc-agents` (per Decision 6: 4 stable task types + swarm InProcess belong together)
- **NEW** `cc-http` (per Decision 5: shared mTLS client builder for cc-api + cc-mcp)
- **MERGED** `cc-commands` into `cc-tui` (slash commands are TUI-specific; `--print` mode doesn't use them)
- **MERGED** `cc-skills` + `cc-plugins` into `cc-memory` (skills are markdown files discovered like CLAUDE.md; plugin hooks feed into cc-hooks)
- **REMOVED** `cc-analytics` (no-op in Phase 2; a simple module in cc-core suffices)
- Net: 20 → 14 library crates (fewer = faster builds, clearer boundaries)

**Layering rule:** A crate may only depend on crates in lower-numbered layers. No cycles. `cc-core` is the root; `cc-tui` and `cc-bridge` are the leaves.

**Naming:** Keep `cc-*` prefix. Short, descriptive. No `claude-` prefix (reserved for binary).

**Public vs internal:** All crates are workspace-internal (`publish = false`). Only the `claude-cli` binary is distributed.

**Rationale:**

1. **20 crates was too many.** Several spike crates were <100 LOC stubs. Compilation overhead and dependency management outweigh boundary benefits at that granularity.
2. **cc-agents consolidation** follows Decision 6: the 4 task types share framework code, state model, and notification format. Splitting them into 2+ crates adds cross-crate type sharing overhead with no isolation benefit.
3. **cc-http extraction** follows Decision 5: mTLS client construction is shared between API and MCP. A small (~200 LOC) shared crate is cleaner than duplicating or creating a dependency cycle.
4. **Skills/plugins merged into cc-memory** because skill discovery walks the same `.claude/` tree as CLAUDE.md loading (A3 contracts). Plugin hooks are just a source in the hooks config — they feed into cc-hooks, not a separate crate.

**Implications for detailed design:**
- Phase 2 design defines the public trait/type surface of each crate
- `cc-core` trait definitions (Tool, Permission, etc.) are the primary integration points
- Build times should be ~30-40% faster than 20-crate layout (fewer compilation units, less link overhead)

---

## Decision 4 — The 80% Feature Cut

**Status:** DECIDED (2026-04-09)

**Question:** Which specific features are in-scope for the rewrite, and which
are explicitly deferred?

**Phase 1 said:** "80% feature parity" with a rough list in `RUST_REWRITE_PLAN.md`
§1 and "refined during development". **This is not acceptable for Phase 2.**
Detailed design cannot start until the list is explicit and enumerated.

**What this decision must produce:**

1. **In-scope list** — every feature that must work in Phase 2, described
   precisely enough that an exit criterion can be written for it.
2. **Out-of-scope list** — every feature explicitly deferred, with a reason
   (post-1.0 / never / blocked on X).
3. **Ambiguous list resolved** — for every TS-version feature that's not
   obviously in or out, make a call.

**Suggested source material:**
- `src/commands/` (slash commands — which are core vs. extras)
- `src/tools/` (built-in tools — which ship in Phase 2)
- `src/services/mcp/` (MCP features — which transports, which capabilities)
- `src/utils/hooks.ts` (hook types — all four, or subset)
- `src/utils/settings/` (settings fields — which are honored)
- TS-side features with no Rust spike coverage (stubs in `cc-tasks`, `cc-agent`)

**Decision:** Explicit in-scope and out-of-scope lists below.

### IN SCOPE — Must work for Phase 2 completion

**CLI Modes:**
- `claude` (interactive TUI)
- `claude --message "..."` (headless single-turn)
- `claude --print "..."` (SDK/bridge mode, JSON output)
- `claude --resume <id>` / `claude --continue` (session resume)
- `--model`, `--output`, `--max-tokens`, `--bypass-permissions`, `--non-interactive`, `--verbose`

**API Integration:**
- Anthropic Messages API with SSE streaming
- Multi-turn conversation with system prompt
- Tool use (tool_choice, tool_result)
- Cache control (ephemeral, global, org scope)
- Usage tracking (input_tokens, output_tokens, cache tokens)
- Cost calculation and display
- Model context window detection + 1M context opt-in
- OAuth authentication (macOS Keychain)
- `ANTHROPIC_API_KEY` env var authentication
- Rate limit handling with retry/backoff

**Built-in Tools (12):**
- Bash, Read, Write, Edit, Glob, Grep, WebFetch, WebSearch
- AgentTool (spawns local_agent)
- TaskCreate, TaskUpdate, TaskList, TaskGet, TaskStop, TaskOutput (task management)
- TodoWrite (todo list management)
- SendMessage (teammate communication)
- TeamCreate, TeamDelete (swarm management)
- EnterPlanMode, ExitPlanMode
- EnterWorktree, ExitWorktree
- AskUserQuestion
- SleepTool
- ToolSearchTool (deferred tool schema loading)

**Session Management:**
- JSONL transcript persistence
- Session resume (`--resume`, `--continue`)
- Session list and selection
- Auto-compact with token threshold (per Decision 7)

**MCP (per Decision 5):**
- stdio transport
- SSE transport (HTTP SSE)
- Streamable HTTP transport
- mTLS support (env var driven)
- `tools/list`, `tools/call`, `initialize`

**Hooks (per A2):**
- All 27 hook events
- `command` and `http` hook types
- Exit code semantics (0/2/other)
- JSON stdin input, JSON stdout response
- `async`, `asyncRewake`, `once`, `if` fields
- Trust enforcement, managed-only policy

**Permission System:**
- Allow/deny/ask rules from settings
- Per-tool permission prompts
- Auto-mode classifier (bypass-permissions, dangerous mode)
- Permission delegation for teammates

**Project Discovery (per A3):**
- Git root walk-up
- Canonical root (worktree resolution)
- `.claude/` directory structure
- Settings merging (6-layer)
- CLAUDE.md walk-up loading (all 6 memory types)
- Rules files with frontmatter globs
- @-include resolution

**Task System (per A1):**
- `local_bash` (background subprocess)
- `local_agent` (in-process sub-agent)
- `in_process_teammate` (swarm InProcessBackend)
- `remote_agent` (HTTP polling)

**Slash Commands (core set):**
- `/help`, `/clear`, `/compact`, `/cost`, `/exit`
- `/resume`, `/session`
- `/permissions`, `/config`
- `/plan`, `/tasks`
- `/memory`, `/skills`
- `/status`, `/version`
- `/diff`, `/commit`
- `/context`
- User-defined skills (markdown-based, same frontmatter as TS)

**Other:**
- Memory files (`~/.claude/memory/`)
- Git integration (status, diff, commit via tools)
- Managed/policy settings (macOS path)
- Custom keybindings (`~/.claude/keybindings.json`)
- `--add-dir` for additional working directories

### OUT OF SCOPE — Explicitly deferred

| Feature | Reason |
|---------|--------|
| `prompt` and `agent` hook types | Post-core; need LLM sub-call infrastructure for hooks |
| JS/TS plugin system | Post-1.0; Rust has no JS runtime |
| Windows and Linux support | Post-macOS; macOS-first constraint |
| PowerShell hook support | Tied to Windows |
| `ws`, `ws-ide`, `sse-ide`, `sdk`, `claudeai-proxy` MCP transports | Per Decision 5 deferral |
| `tmux`/`iTerm` swarm backends | Per Decision 6; InProcessBackend only |
| `local_workflow`, `monitor_mcp`, `dream` task types | Per Decision 6; experimental/feature-flagged |
| Full telemetry/analytics system | No-op initially; add post-1.0 |
| `REPLTool`, `NotebookEditTool`, `LSPTool` | Advanced/niche tools; post-1.0 |
| `ScheduleCronTool`, `RemoteTriggerTool` | Cloud-dependent features |
| `McpAuthTool`, `ReadMcpResourceTool`, `ListMcpResourcesTool` | MCP resource/auth features; post-core |
| `SyntheticOutputTool`, `BriefTool`, `ConfigTool`, `SkillTool` | Internal/helper tools; add as needed |
| ~60 non-core slash commands (bughunter, ultraplan, review, teleport, stickers, voice, vim, chrome, etc.) | Plugin/enterprise features |
| Bedrock/Vertex API backends | Anthropic direct API only for Phase 2 |
| Remote settings sync | Policy file-based only for Phase 2 |
| Team memory sync | Post-1.0 |
| IDE integrations (VS Code, JetBrains extensions) | Separate project |
| Desktop app (Electron) | Separate project |
| Auto-update / upgrade mechanism | Post-1.0 |

### AMBIGUOUS → RESOLVED

| Feature | Decision | Reason |
|---------|----------|--------|
| `PowerShellTool` | OUT | macOS-only Phase 2 |
| Theme/color support | IN (basic) | Simple to implement; user-visible quality |
| `--output json` structured output | IN | SDK users need it |
| Cost display in TUI | IN | Core daily-driver feature |
| Auto-memory (session memory compaction) | IN | Part of auto-compact flow |
| `EnterWorktree`/`ExitWorktree` | IN | Agent isolation feature |

**Rationale:**

The cut follows the principle: **everything needed for daily-driver use on macOS** is in scope. Enterprise features (Bedrock, Vertex, remote sync, IDE), platform features (Windows, Linux), and experimental features (workflow scripts, monitor MCP, dream) are deferred. The in-scope list covers ~80% of what a typical developer uses; the out-of-scope list is the long tail of enterprise, platform, and experimental features.

**Implications for detailed design:**
- Test matrix: ~12 core tools + 27 hook events + 3 MCP transports + 4 task types + ~15 slash commands
- Acceptance criteria: one per in-scope bullet point
- Crate surface area validated against in-scope list (all covered by 14-crate layout)

---

## Decision 5 — MCP Transports + mTLS  *(DECIDED 2026-04-08)*

**Status:** DECIDED

**Phase 1 framing error corrected:** `02-compatibility-contracts.md` §Category 6
listed 4 MCP transport types. Code review of `src/services/mcp/client.ts:619-868`
found **9 transport types**: `stdio`, `sse`, `sse-ide`, `http` (Streamable HTTP),
`ws`, `ws-ide`, `sdk`, `claudeai-proxy`. Additionally, mTLS (`src/utils/mtls.ts`)
is **orthogonal** to transport — it applies to `sse`, `http`, `ws`, `ws-ide` via
`getMTLSAgent()` / `getWebSocketTLSOptions()` / `getFetchOptions()`, not just to
WebSocket as Phase 1 implied.

Decision 5 is therefore split into three sub-decisions.

### 5a — Transport scope

**IN SCOPE for Phase 2:**
- `stdio` — local MCP server (subprocess + stdin/stdout)
- `sse` — remote HTTP SSE
- `http` — Streamable HTTP (current MCP spec)

**DEFERRED to post-1.0:**
- `ws`, `ws-ide` — `rmcp` crate WebSocket support is weaker than stdio/SSE; IDE WebSocket variant depends on IDE integration work
- `sse-ide` — IDE integration path, tied to VS Code / JetBrains extension work
- `sdk` — in-process SDK embedding; Rust will have its own embedding story
- `claudeai-proxy` — claude.ai login-state proxy, high complexity

### 5b — mTLS support

**IN SCOPE for Phase 2**, applied to `sse` + `http` (the only HTTP-based transports in scope per 5a).

**Env var contract — must match TS names exactly:**
- `CLAUDE_CODE_CLIENT_CERT` — path to client cert (PEM)
- `CLAUDE_CODE_CLIENT_KEY` — path to client key (PEM)
- `CLAUDE_CODE_CLIENT_KEY_PASSPHRASE` — key passphrase
- `SSL_CERT_FILE` — extra CA certs (replaces Node's `NODE_EXTRA_CA_CERTS`; SSL_CERT_FILE is the Rust/OpenSSL-conventional name and is already honored by `rustls-native-certs` / `openssl`)

**Implementation guidance:** `reqwest::Identity::from_pem` + `ClientBuilder::identity` for the client cert; `ClientBuilder::add_root_certificate` for the extra CAs. The mTLS concern belongs in `cc-api` / `cc-mcp` at the HTTP-client construction layer, not in a separate crate.

### 5-other — Unscoped transports

`ws`, `ws-ide`, `sse-ide`, `sdk`, `claudeai-proxy` are explicitly **DEFERRED**. A decision to add any of them post-1.0 should trigger a new mini-RFC and likely a new crate or transport plugin point — they are not to be "quietly added" later.

### Rationale

Adopting the recommendation as-is (rather than "do all 9") because: (i) `stdio` + `sse` + `http` cover the common public MCP server ecosystem; (ii) `rmcp`'s WebSocket support is immature — adopting `ws` would likely require a custom transport layer, which is out of proportion to its real usage; (iii) mTLS is a real enterprise need and is cheap to implement correctly in Rust; (iv) the IDE transports (`*-ide`, `sdk`) are tied to integration work that isn't on the Phase 2 critical path.

### Implications for detailed design

- `cc-mcp` exposes exactly 3 transports; interface must leave room for adding a 4th+ without reshaping the public API
- `cc-api` + `cc-mcp` share a common HTTP client construction layer (mTLS-aware), so likely a small `cc-http` or equivalent is justified — register as an input to Decision 3 (crate layout)
- ~~`02-compatibility-contracts.md` §Category 6 must be updated~~ — **DONE 2026-04-08** (9 transports listed with Phase 2 scope column)
- The env var names above become a MUST MATCH EXACTLY compatibility contract — users' existing env configs work unchanged

---

## Decision 6 — Multi-Agent / Task Subsystem Scope  *(DECIDED 2026-04-08)*

**Status:** DECIDED

**Phase 1 factual error corrected:** `04-risk-inventory.md` Risk 10 claimed
"The TS version uses Unix Domain Sockets for inter-agent communication." This is
**false**. Full-source search for `UnixListener` / `UnixStream` / `.sock` /
`socketPath` / `net.createServer.*unix` found zero MCP/agent-related hits. The
real TS multi-agent architecture is multi-modal:

| TS `TaskType` | Mechanism | Source | Status in TS |
|---|---|---|---|
| `local_bash` | Background subprocess (pipe) | `src/tasks/LocalShellTask/` | Stable |
| `local_agent` | **In-process**, independent query loop, same address space | `src/tools/AgentTool/runAgent.ts` | Stable |
| `in_process_teammate` | **In-process**, multi-teammate via shared `AppState` | `src/utils/swarm/` (~4.1K LOC) | Stable |
| `remote_agent` | **HTTP polling** of Anthropic-side remote session API | `src/tasks/RemoteAgentTask/` | Stable |
| `local_workflow` | Workflow scripts | `src/tasks/` | Feature-flagged (`WORKFLOW_SCRIPTS`), experimental |
| `monitor_mcp` | MCP monitoring | `src/tasks/` | Feature-flagged (`MONITOR_TOOL`), experimental |
| `dream` | Background inference | `src/tasks/DreamTask/` | Experimental |

There is **no Unix Domain Socket IPC anywhere in the TS codebase**. The
"Multi-agent UDS IPC protocol design" item is deleted from this plan.

### Decision

**IN SCOPE for Phase 2 (depth contracts in A1):**
- `local_bash` (already covered by Bash tool's background mode)
- `local_agent` (AgentTool / Task tool — in-process sub-agent)
- `in_process_teammate` (teammate model body only — see 6-b for UI layer)
- `remote_agent` (HTTP polling of Anthropic remote session API)

**DEFERRED to post-1.0 (with explicit re-evaluation trigger):**
- `local_workflow`
- `monitor_mcp`
- `dream`

**Re-evaluation trigger for the deferred 3:**
> Revisit inclusion in a future Phase if and when *all* of the following are true
> in the upstream TS source: (a) the task's core behavior has remained unchanged
> for ≥ 3 months, (b) its feature flag has flipped to default-on (or been
> removed), (c) a user request or operational need for it has been surfaced.

**DELETED from scope (factual error):**
- Multi-agent UDS IPC protocol — never existed in TS, remove from all planning docs.

### Sub-decisions

**6-a — Feature-flagged experimental tasks:** Option (iii) — only the 4 stable
tasks get depth contracts in A1; the 3 experimental tasks are deferred with the
re-evaluation trigger above. *(Rationale: contracts written against moving
upstream code either go stale or must be written so loosely they carry no
design value. Quality = depth on stable behavior, not breadth on unstable
behavior.)*

**6-b — Swarm UI multiplexer backends (`tmux`, `iTerm`, `InProcess`, `Pane`):**
Only `InProcessBackend` is in scope — teammates share the main Rust TUI. The
`tmux` and `iTerm` backends are explicitly deferred; any pane-splitting UX
decision is left to Phase 2 TUI design (Decision 2 + `phase2-design.md`), not
to A1 contract work. *(Rationale: tmux/iTerm integration is a UX layer calling
external CLI tools, not a behavior contract. The teammate body's task model,
state sync, messaging, and shutdown semantics are what A1 must nail down — the
visual pane layout is a separate design concern.)*

**6-c — Decision 3 constraint registration:** The in-scope set
(`local_agent` + `in_process_teammate` + `remote_agent` + `local_bash` +
swarm's InProcess backend) is large enough that Decision 3 (crate layout)
**must** consider a dedicated crate for the swarm/agent subsystem. Registered
in the Decision 3 section below as an input constraint.

### Rationale (overall)

User direction: "quality > time/LOC". The quality-maximizing split for contract
work on a rewrite is **depth on stable behavior + explicit deferral of unstable
behavior with a re-evaluation trigger**, not broad-but-shallow coverage.
Skeleton-only contracts for the 3 experimental tasks were rejected because
placeholder interfaces tend to become accidental constraints when the real
implementation arrives.

### Implications for detailed design

- **A1 gap patch** (`03b-behavior-contracts-tasks-agent.md`) covers 4 stable
  task types with full depth: lifecycle state machine, cancellation/abort
  semantics, output streaming, parent↔child message flow (in-process), and
  `remote_agent` polling protocol. Expected size ~400–600 lines.
- **`cc-swarm` / `cc-agents` crate candidate** registered in Decision 3.
- ~~**`04-risk-inventory.md` Risk 10** must be rewritten~~ — **DONE 2026-04-08** (rewritten as "Heterogeneous Concurrency Model", score upgraded LOW 2 → HIGH 18).
- **Terminal multiplexer integration** (tmux/iTerm) is not a Phase 2 feature.
  Document this explicitly in Decision 4 (80% feature cut) under "out of scope".
- **`agent` hook type** (one of the 4 hook kinds from Category 8) must be
  specified in A2 (hooks lifecycle), because it interacts with `local_agent`
  and `remote_agent` lifecycles.

---

## Decision 7 — Tokenizer Parity Strategy

**Status:** DECIDED (2026-04-09)

**Question:** How will the Rust version count tokens for the auto-compact
threshold, given that Anthropic does not publish its tokenizer and
`tiktoken-rs` is for OpenAI models?

**Why this is a Phase 2 entry gate:**
`03-behavior-contracts.md` §C1 classifies the auto-compact threshold as
**MUST REPLICATE EXACTLY** (`autoCompactThreshold = effectiveContextWindow - 13,000`).
But exact replication requires an exact token count, and the Rust ecosystem
has no drop-in equivalent to the TS version's tokenizer. This contradiction
must be resolved before `cc-query` / `cc-api` interfaces are designed, because
the chosen strategy shapes those interfaces directly.

**Options to consider:**

1. **Use Anthropic's `/v1/messages/count_tokens` endpoint.**
   - Pros: Exact parity with Anthropic's own counting.
   - Cons: Extra HTTP round-trip on every threshold check (latency + rate limit + cost impact). Requires network availability before compaction decisions.
   - Interface impact: `cc-query` compaction check becomes `async`; `cc-api` must expose a `count_tokens` call.

2. **Estimate locally with a known-approximate tokenizer + safety margin.**
   - Pros: No network call; compaction check stays synchronous.
   - Cons: Off by some percentage; may trigger early (wasted context) or late (API error from overflow).
   - Interface impact: keep compaction sync; add an explicit `COMPACT_SAFETY_MARGIN_TOKENS` config knob; downgrade §C1 classification from MUST EXACT to ACCEPTABLE VARIANCE with documented margin.

3. **Hybrid — local estimate for the warning/early check, remote count for the hard threshold.**
   - Pros: Avoids most round-trips; falls back to exact count only when close to the threshold.
   - Cons: More moving parts; two tokenizer paths to maintain.
   - Interface impact: `cc-query` needs both sync-estimate and async-confirm entry points.

4. **Use Anthropic's token count from the previous response's `usage.input_tokens`.**
   - Pros: Free — Anthropic already returns it on every response. Exact by definition.
   - Cons: Only tells you "what was counted last time" — doesn't cover tokens added since the last API call (e.g., tool results queued before next send). Needs a small local add-on for the delta.
   - Interface impact: `cc-api` stream must surface `usage` cleanly; `cc-query` keeps a running count.

**Decision:** Option 4 — Reuse `usage.input_tokens` from previous API response + local rough delta estimate. This is **exactly what the TS version does**.

**Key finding from TS source analysis:**

The TS version's `tokenCountWithEstimation()` (`src/utils/tokens.ts:226-261`) works as follows:
1. Walks backwards through messages to find the most recent `assistant` message with `.usage` data (stored from every API response at `src/services/api/claude.ts:2246`)
2. Computes anchor: `usage.input_tokens + cache_creation_input_tokens + cache_read_input_tokens + usage.output_tokens`
3. Adds `roughTokenCountEstimationForMessages()` for any messages added after the last API response (delta)
4. The rough estimation uses `content.length / 4` for text, fixed 2000 for images/documents, recursive estimation for tool results (`src/services/tokenEstimation.ts:203-435`)
5. **Never calls `/v1/messages/count_tokens` for auto-compact threshold checks** — the API endpoint is only used for compaction itself, manual verification, and tool result sizing

The auto-compact threshold formula (`src/services/compact/autoCompact.ts:32-91`):
- `effectiveContextWindow = contextWindow - min(maxOutputTokens, 20_000)`
- `threshold = effectiveContextWindow - 13_000` (AUTOCOMPACT_BUFFER_TOKENS)
- Override: `CLAUDE_CODE_AUTO_COMPACT_WINDOW` env var caps `contextWindow`

**Rationale:**

The original Phase 2 entry gate framed this as a contradiction ("MUST REPLICATE EXACTLY requires an exact token count, but Rust has no tokenizer"). This framing was based on a **false premise** — the TS version itself does not use an exact tokenizer for the threshold check. It uses a hybrid of API-provided usage data + character-length heuristic. The Rust version can replicate this approach identically:

1. Store `usage` from every API response (already needed for cost tracking)
2. Implement `rough_token_estimate(content) → usize` as `content.len() / 4` with special cases for JSON (÷2), images (2000 fixed), etc.
3. `should_auto_compact()` stays **synchronous** — no network call needed
4. The 13,000-token buffer absorbs estimation error (the TS version accepts the same ±10% error)

**§C1 reclassification:** The auto-compact threshold formula itself (constants, env var override) remains **MUST REPLICATE EXACTLY**. The token counting method is reclassified from MUST REPLICATE EXACTLY to **MUST REPLICATE APPROACH** — meaning: use the same hybrid strategy (API usage anchor + rough delta), not necessarily byte-identical counts. Acceptable variance: ±10% on the delta estimate, absorbed by the 13k buffer.

**Implications for detailed design:**
- `cc-api`: Stream must surface `Usage { input_tokens, output_tokens, cache_creation_input_tokens, cache_read_input_tokens }` on every completed response. No `count_tokens` method needed for auto-compact.
- `cc-query`: `should_auto_compact(&self) -> bool` stays synchronous. Maintains a running token count anchored to last API usage + rough delta.
- `cc-query`: Needs `rough_token_estimate(content: &ContentBlock) -> usize` utility (simple, ~50 LOC)
- `03-behavior-contracts.md` §C1: Reclassify token counting from MUST EXACT to MUST REPLICATE APPROACH
- `04-risk-inventory.md`: Add Risk 11 (Tokenizer Parity) at **LOW** severity — the "risk" was based on a false premise; the actual TS approach is trivially replicable in Rust
- Env vars to honor: `CLAUDE_CODE_AUTO_COMPACT_WINDOW` (context window cap)

---

## Phase 2 Entry Checklist

Phase 2 (detailed design) may begin when **all** of the following are true:

**Decisions (7 total):**
- [x] Decision 1 (Async Runtime) — **DECIDED 2026-04-09** (Tokio multi-threaded)
- [x] Decision 2 (TUI Library) — **DECIDED 2026-04-09** (Ratatui + crossterm)
- [x] Decision 3 (Crate Layout) — **DECIDED 2026-04-09** (14 library crates + 1 binary; consolidated from 20)
- [x] Decision 4 (80% Feature Cut) — **DECIDED 2026-04-09** (explicit in/out-of-scope lists with ambiguous items resolved)
- [x] Decision 5 (MCP Transports + mTLS) — **DECIDED 2026-04-08** (3 transports in scope + mTLS cross-cutting; 5 others deferred)
- [x] Decision 6 (Multi-Agent / Task Subsystem Scope) — **DECIDED 2026-04-08** (4 stable tasks in scope; 3 experimental deferred; UDS IPC deleted as factual error)
- [x] Decision 7 (Tokenizer Parity Strategy) — **DECIDED 2026-04-09** (Option 4: reuse usage.input_tokens + rough delta; §C1 reclassified to MUST REPLICATE APPROACH)

**Phase 1 gap patches:**
- [x] **A3** — `.claude/plan/02b-project-discovery.md` written — **DONE 2026-04-09** (project root resolution, `.claude/` walk-up, CLAUDE.md loading, settings merging, security)
- [x] **A2** — `.claude/plan/03c-behavior-contracts-hooks.md` written — **DONE 2026-04-09** (27 hook events, 5 types, execution semantics, ordering, special fields, trust, env vars)
- [x] **A1** — `.claude/plan/03b-behavior-contracts-tasks-agent.md` written — **DONE 2026-04-09** (4 stable task types with lifecycle, cancellation, streaming, communication contracts)
- [x] **B2 follow-up** — **DONE 2026-04-09** (`04-risk-inventory.md` updated with Risk 11 at LOW; `03-behavior-contracts.md` §C1 reclassified per Decision 7)

**Alignment & closeout:**
- [x] `RUST_REWRITE_PLAN.md` §2 checkboxes updated to match — **DONE 2026-04-08** (now 7 items: 4 unresolved + 2 decided + 1 new tokenizer)
- [x] `01-known-context.md` Open Decisions list reconciled — **DONE 2026-04-09** (all 7 decisions + Strangler + Auth + UDS IPC updated)
- [x] Spike audit complete — **DONE 2026-04-09** (see table below)
- [x] New file `.claude/plan/phase2-design.md` opened — **DONE 2026-04-09**

---

## Spike Audit — Per-Crate Classification (2026-04-09)

Each spike crate classified as REUSE (adopt with minor edits), REWRITE (valuable
reference but needs redesign), or DISCARD (not useful for Phase 2).

| Spike Crate | LOC | Phase 2 Target | Verdict | Notes |
|-------------|-----|----------------|---------|-------|
| `cc-core` | 293 | cc-core | **REUSE** | Types/traits are solid; add new types from A1/A2 contracts |
| `cc-config` | 389 | cc-config | **REWRITE** | Missing A3 contracts: walk-up, settings merge, security exclusions. Structure OK, logic incomplete. |
| `cc-analytics` | 13 | (module in cc-core) | **DISCARD** | No-op stub; not worth a crate. Add a `pub mod analytics {}` in cc-core. |
| `cc-auth` | 156 | cc-auth | **REUSE** | macOS Keychain + API key working; add OAuth refresh if needed |
| `cc-api` | 536 | cc-api | **REWRITE** | SSE streaming works but needs: usage extraction (Decision 7), cc-http shared client (Decision 5), proper error types |
| `cc-permissions` | 144 | cc-permissions | **REUSE** | Allow/deny/ask rules are correct; add auto-mode classifier |
| `cc-tools` | 1105 | cc-tools | **REUSE** | 8 core tools implemented; add task tools from A1 |
| `cc-hooks` | 501 | cc-hooks | **REWRITE** | Has basic command hooks; A2 contracts require 27 events, 5 hook types, full execution semantics, trust, dedup, snapshot |
| `cc-git` | 138 | cc-git | **REWRITE** | Basic git root; missing A3 contracts: canonical root, worktree security, bare repo detection |
| `cc-mcp` | 973 | cc-mcp | **REUSE** | stdio + HTTP SSE working; add cc-http extraction, Streamable HTTP transport |
| `cc-memory` | 182 | cc-memory | **REWRITE** | Basic loader; missing A3 walk-up, rules frontmatter, @-includes, 6 memory types. Will also absorb cc-skills + cc-plugins. |
| `cc-session` | 384 | cc-session | **REUSE** | JSONL transcript + TS-format resume working |
| `cc-query` | 538 | cc-query | **REWRITE** | Tool loop works but needs: Decision 7 token tracking, proper auto-compact, task attachments from A1 |
| `cc-bridge` | 226 | cc-bridge | **REUSE** | SDK `--print` path working |
| `cc-skills` | 345 | (merge into cc-memory) | **DISCARD** | Functionality absorbed into cc-memory per Decision 3 |
| `cc-plugins` | 249 | (merge into cc-memory) | **DISCARD** | Functionality absorbed into cc-memory per Decision 3 |
| `cc-commands` | 638 | (merge into cc-tui) | **DISCARD** | Slash commands merge into cc-tui per Decision 3 |
| `cc-tui` | 1405 | cc-tui | **REWRITE** | Abandoned in place (state.md). Structural approach OK but incomplete. Redesign from Phase 2 component tree. |
| `cc-tasks` | 1 | (merge into cc-agents) | **DISCARD** | Empty stub; cc-agents is new crate per Decision 3 |
| `cc-agent` | 1 | (merge into cc-agents) | **DISCARD** | Empty stub; cc-agents is new crate per Decision 3 |

**Summary:** 6 REUSE, 6 REWRITE, 8 DISCARD (5 merged into other crates, 3 empty/stub)

**New crates not in spike:** `cc-http` (Decision 5), `cc-agents` (Decision 3/6)

---

## Working Notes

*(Decisions and gap patches complete. This section preserved for Phase 2 design discussions.)*
