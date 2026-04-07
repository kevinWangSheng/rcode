# Strategy Decision — Phase 5

## Strategy Decision

**Chosen strategy:** Hybrid (Phased Cutover)

**Primary reason:** The TS codebase is tightly coupled (React/Ink TUI owns the main loop), making
Strangler Fig impractical, but the highest risk area (TUI, score=27) needs validation before
committing to full implementation — so a pure Big Bang risks wasted work if the TUI Spike fails.

**Supporting factors:**
- TUI is the highest-risk item (score=27); Hybrid lets us run the TUI Spike before building the
  full TUI, so failure is discovered early at low cost
- The 80% parity goal maps naturally to phases: core API → tools → TUI → extras
- Learning goal is served by building incrementally (each phase teaches a new layer)
- macOS-first constraint means no cross-platform compatibility shim needed during development
- No external user migration pressure — phased delivery is for developer validation, not production rollout

**Acknowledged tradeoffs:**
- Phase 1 binary (headless, no TUI) is not useful as a daily driver
- More coordination required between phases (each phase's interfaces become contracts)
- Phase 3 (TUI) is still effectively a Big Bang for the presentation layer

**Phase structure:**
- Phase 1: cc-core, cc-config, cc-auth, cc-api → headless binary (`claude --no-tui` / JSON output)
- Phase 2: cc-session, cc-tools, cc-mcp, cc-hooks, cc-git, cc-memory → full tool execution, no TUI
- Phase 3: TUI Spike first → cc-tui, cc-commands, cc-query → interactive CLI
- Phase 4: cc-tasks, cc-agent, cc-skills, cc-plugins, cc-bridge, cc-analytics → remaining 80% features

**Impact on build order (Phase 6):**
- Layer 0 crates (no dependencies) must be buildable and testable independently
- Each phase boundary is a clean interface — Phase N+1 crates depend only on Phase N outputs
- Parallel tracks within each phase are maximized (e.g., cc-tools and cc-mcp can be built simultaneously in Phase 2)
- TUI Spike (standalone binary) must complete and pass acceptance criteria before Phase 3 begins

**Output filename:** `RUST_REWRITE_PLAN.md` (single plan file, no versioning)
