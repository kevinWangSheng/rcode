# Dependency Graph — Phase 6

Based on actual TypeScript import analysis via subagent exploration.

---

## Circular Dependencies Found (TS → Rust Resolution)

The TypeScript source has 3 circular dependency clusters that must be broken in the Rust design:

| Cycle | TS Cause | Rust Resolution |
|-------|----------|-----------------|
| `cc-api` ↔ `cc-mcp` | `api.ts` calls MCP for resource prefetch; `mcp/client.ts` calls `api.ts` for tool schemas | Break via `cc-core` traits: tools register via `Tool` trait; `cc-api` accepts `&dyn Tool` slices; MCP tools implement same trait |
| `cc-api` ↔ `cc-tools` | `api.ts` calls `getTools()` to build tool list; tools import API types | Same resolution: `Tool` trait in `cc-core`; `cc-api` receives tool list at call site from `cc-query` |
| `cc-permissions` ↔ `cc-tools` | `permissions.ts` imports tool-specific rules; tools import `PermissionResult` | `PermissionResult` and `PermissionRule` types moved to `cc-core`; `cc-permissions` implements logic; tools use `cc-core` types only |

**Key principle:** `cc-core` defines all shared traits and result types. Higher layers implement them.

---

## Dependency Matrix (resolved for Rust)

| Crate | Depends On |
|-------|------------|
| cc-core | (none) |
| cc-config | cc-core |
| cc-analytics | cc-core |
| cc-git | cc-core |
| cc-auth | cc-core, cc-config |
| cc-permissions | cc-core, cc-config |
| cc-tools | cc-core, cc-config, cc-permissions |
| cc-hooks | cc-core, cc-config, cc-permissions |
| cc-mcp | cc-core, cc-config, cc-auth, cc-tools |
| cc-api | cc-core, cc-config, cc-auth, cc-permissions |
| cc-session | cc-core, cc-config, cc-api |
| cc-memory | cc-core, cc-config |
| cc-agent | cc-core, cc-session |
| cc-tasks | cc-core, cc-session |
| cc-skills | cc-core, cc-config, cc-hooks |
| cc-plugins | cc-core, cc-config, cc-skills |
| cc-commands | cc-core, cc-skills, cc-plugins, cc-mcp |
| cc-query | cc-core, cc-config, cc-api, cc-session, cc-tools, cc-mcp, cc-hooks, cc-permissions |
| cc-bridge | cc-core, cc-config, cc-api, cc-query |
| cc-tui | cc-core, cc-api, cc-commands, cc-hooks, cc-memory, cc-query |

No circular dependencies in the resolved Rust design.

---

## Build Layers

### Layer 0 — No dependencies (build first, fully parallel)
- **cc-core**

### Layer 1 — Depends only on Layer 0 (parallel)
- **cc-config** (depends on: cc-core)
- **cc-analytics** (depends on: cc-core)
- **cc-git** (depends on: cc-core)

### Layer 2 — Depends on Layer 0–1 (parallel)
- **cc-auth** (depends on: cc-core, cc-config)
- **cc-permissions** (depends on: cc-core, cc-config)
- **cc-memory** (depends on: cc-core, cc-config)

### Layer 3 — Depends on Layer 0–2 (parallel)
- **cc-tools** (depends on: cc-core, cc-config, cc-permissions)
- **cc-hooks** (depends on: cc-core, cc-config, cc-permissions)

### Layer 4 — Depends on Layer 0–3 (parallel)
- **cc-mcp** (depends on: cc-core, cc-config, cc-auth, cc-tools)
- **cc-api** (depends on: cc-core, cc-config, cc-auth, cc-permissions)

### Layer 5 — Depends on Layer 0–4 (parallel)
- **cc-session** (depends on: cc-core, cc-config, cc-api)

### Layer 6 — Depends on Layer 0–5 (parallel)
- **cc-agent** (depends on: cc-core, cc-session)
- **cc-tasks** (depends on: cc-core, cc-session)
- **cc-skills** (depends on: cc-core, cc-config, cc-hooks)

### Layer 7 — Depends on Layer 0–6 (parallel)
- **cc-plugins** (depends on: cc-core, cc-config, cc-skills)

### Layer 8 — Depends on Layer 0–7 (parallel)
- **cc-commands** (depends on: cc-core, cc-skills, cc-plugins, cc-mcp)

### Layer 9 — Depends on Layer 0–8 (single — the orchestrator)
- **cc-query** (depends on: cc-core, cc-config, cc-api, cc-session, cc-tools, cc-mcp, cc-hooks, cc-permissions)

### Layer 10 — Depends on Layer 0–9 (parallel — final frontends)
- **cc-bridge** (depends on: cc-core, cc-config, cc-api, cc-query)
- **cc-tui** (depends on: cc-core, cc-api, cc-commands, cc-hooks, cc-memory, cc-query)

---

## Parallelization Map (by Hybrid phase)

### Phase 1 crates (headless binary)
```
Track A: cc-core → cc-config → cc-auth → cc-api
                              └→ cc-analytics (parallel to cc-auth)
```
Deliverable: `claude --no-tui` that can send a message and stream the response.

### Phase 2 crates (tool execution)
```
Track A: cc-permissions → cc-tools
Track B: cc-permissions → cc-hooks (parallel to Track A)
Track C: cc-mcp (after cc-tools)
Track D: cc-memory (parallel to all, depends only on Layer 2)
Track E: cc-session (after cc-api from Phase 1)
Track F: cc-git (parallel, Layer 1)
→ cc-query (after all Phase 2 tracks)
→ cc-tasks, cc-agent (after cc-session)
```
Deliverable: Headless binary with full tool execution, MCP, session resume.

### Phase 3 crates (TUI — after Spike passes)
```
TUI Spike first (standalone binary, not a crate)
Then: cc-skills → cc-plugins → cc-commands → cc-tui
```
Deliverable: Interactive `claude` binary, daily-driver usable.

### Phase 4 crates (remaining features)
```
cc-bridge (parallel to cc-tui)
Remaining: analytics integration, task management polish
```
Deliverable: 80% feature parity with TS version.

---

## Critical Path

```
cc-core → cc-config → cc-auth → cc-api → cc-session → cc-query → cc-tui
```

**Length:** 7 layers deep (Layer 0 → Layer 10 via the longest chain)

The critical path runs through the API and session layers. `cc-permissions` and `cc-tools`
are on a parallel track that merges into `cc-query`, but the API→session→query chain
is the primary constraint.

---

## Notes

- `cc-analytics` is a cross-cutting concern (Layer 1) but kept separate to avoid polluting other crates
- `cc-git` (Layer 1) is deliberately isolated — git operations have no business logic deps
- `cc-memory` (Layer 2) is simpler than expected — just filesystem scanning + config path
- The `Tool` trait in `cc-core` is the key architectural decision that breaks all three circular deps
