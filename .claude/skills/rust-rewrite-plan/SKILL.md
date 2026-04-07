---
name: rust-rewrite-plan
description: "Drive the planning phase of rewriting Claude Code from TypeScript to Rust. Use when starting, resuming, or completing any phase of the Rust rewrite planning process. Combines Inversion (gather context first), Pipeline (sequential phases with gates), Reviewer (checklist-based phase completion), and Generator (final document synthesis)."
allowed-tools: Read, Write, Glob, Grep, Bash, WebSearch, Agent
argument-hint: "[phase number to run, e.g. 'phase-1' or 'all' or 'status']"
disable-model-invocation: true
---

# /rust-rewrite-plan — Rust Rewrite Planning Driver

You are driving the **planning phase** of rewriting Claude Code (TypeScript/Bun/React) into Rust.
Planning produces strategic decisions and constraints — NOT implementation code, NOT type definitions, NOT crate-level specs.

---

## ⚠️ CONSTITUTIONAL RULES (MUST / NEVER — no exceptions)

```
NEVER write Phase 8 (synthesis) until Phases 1–7 output files ALL exist and pass their gates.
NEVER declare a phase "complete" based on memory — the output file is the only evidence.
NEVER read more than 3 source files directly — use Agent(Explore) for any broader exploration.
NEVER make the Big Bang / Strangler Fig decision before Phase 3 (behavior contracts) is done.
NEVER skip a Reviewer gate by saying "this phase is obvious" or "we already discussed this."
MUST check .claude/plan/state.md at the very start of every session before doing anything.
MUST write to the phase output file before marking that phase complete in state.md.
MUST use Subagent(Explore) for any codebase exploration that spans more than one module.
```

---

## WHAT AGENT DISCOVERS (facts — not invented)

- Module list → from directory structure of source repo
- Dependency relationships → from import/require analysis
- File formats → from reading actual config files
- Existing behaviors → from reading actual source code logic

## WHAT AGENT DECIDES (requires reasoning)

- Which behaviors constitute compatibility contracts
- Which risks need a Spike before committing
- Big Bang vs Strangler Fig strategy
- Milestone gate criteria (measurable, not vague)

---

## STEP 0 — SESSION START (always run first)

1. Read `.claude/plan/state.md` (if it doesn't exist, this is the first run — create it with all phases as `pending`).
2. Report current status:
   ```
   Phase 1: [status] [output file exists? yes/no]
   Phase 2: [status] [output file exists? yes/no]
   ...
   ```
3. If argument is `status` → stop here.
4. If argument is `all` → run all pending phases in sequence.
5. If argument is `phase-N` → run only that phase.
6. If no argument → run the next pending phase.

---

## PHASE 1 — INVERSION: Gather Known Context

**Pattern: Inversion** — agent interviews user before any synthesis.
**Output:** `.claude/plan/01-known-context.md`
**Gate:** Read `references/phase-completion-gates.md` → Section "Phase 1 Gate"

### Instructions

DO NOT write the output file until all questions are answered.

Ask these questions sequentially. Wait for the user's answer before moving to the next.

**Q1.** What is the primary motivation for the Rust rewrite?
(performance / binary distribution / type safety / learning / other — explain)

**Q2.** Are there any non-negotiable constraints?
(e.g., must run on Windows, must support existing MCP servers, must ship within N months)

**Q3.** What has already been decided?
(Read `RUST_REWRITE_PLAN.md` if it exists — summarize what decisions were captured there,
then ask user: "Which of these decisions are confirmed? Which are still open?")

**Q4.** What does "the rewrite is complete" mean to you?
(100% feature parity? Core features only? Performance benchmarks? User acceptance?)

**Q5.** Is there anything about the existing TypeScript code you already know is problematic
or must NOT be replicated in the Rust version?

After all answers collected, write `.claude/plan/01-known-context.md` using this structure:
```markdown
# Known Context

## Primary Motivation
[answer]

## Non-Negotiable Constraints
[answer]

## Confirmed Decisions
[from Q3 — confirmed items only]

## Open Decisions
[from Q3 — still open items]

## Definition of "Complete"
[answer]

## Known Exclusions
[answer]
```

Then run the Reviewer gate (references/phase-completion-gates.md → Phase 1).

---

## PHASE 2 — COMPATIBILITY CONTRACTS (Format-Level)

**Pattern: Reviewer** — enumerate against checklist, not free-form.
**Output:** `.claude/plan/02-compatibility-contracts.md`
**Gate:** Read `references/phase-completion-gates.md` → Section "Phase 2 Gate"

### Instructions

Read `references/compatibility-checklist.md`. For each category in the checklist:

1. Find the actual files in the source repo that define this format/protocol.
2. Document the exact format (do NOT paraphrase — quote the relevant structure).
3. State the compatibility requirement: MUST MATCH EXACTLY / MUST BE READABLE / CAN CHANGE WITH MIGRATION TOOL.

Categories to cover (from checklist):
- Configuration files (settings.json schema)
- Session/history files (JSONL format)
- Memory files (markdown format)
- API message format (Anthropic API wire format)
- MCP protocol (JSON-RPC 2.0 messages)
- Slash command names and argument format
- Hook script interface (env vars, stdin, exit codes)
- Plugin manifest format
- Keybindings file format

Write `.claude/plan/02-compatibility-contracts.md`.
Then run the Reviewer gate.

---

## PHASE 3 — BEHAVIOR CONTRACTS (Runtime-Level)

**Pattern: Pipeline + Subagent Exploration**
**Output:** `.claude/plan/03-behavior-contracts.md`
**Gate:** Read `references/phase-completion-gates.md` → Section "Phase 3 Gate"

### Instructions

Behavior contracts are runtime behaviors that are NOT captured in file formats but MUST be preserved.
They are discovered by reading code, not by asking the user.

For each of the following areas, spawn an Explore subagent (thoroughness: "very thorough"):

**Area A: Permission Dialog Behavior**
- What triggers a permission prompt vs. auto-allow?
- What does Ctrl+C do during a permission prompt?
- What happens if the user ignores a prompt?

**Area B: Streaming Interruption**
- What happens when the user presses Ctrl+C during streaming?
- Is partial output saved? Is the message added to history?
- What is the state of the session after interruption?

**Area C: Context Compaction**
- What triggers auto-compact?
- What is preserved vs. discarded?
- What happens to tool-use results during compaction?

**Area D: Tool Error Handling**
- What does a failed Bash command produce in the conversation?
- Is the error shown to the model? As what type of content?
- Does Claude retry automatically?

**Area E: Session Resume**
- What state is saved to disk after each turn?
- What is NOT saved (ephemeral state)?
- What does `--resume` restore vs. not restore?

For each area: document the behavior as observed in code with file:line references.
Classify each as: MUST REPLICATE EXACTLY / ACCEPTABLE VARIANCE / INTENTIONALLY CHANGING.

Write `.claude/plan/03-behavior-contracts.md`.
Then run the Reviewer gate.

---

## PHASE 4 — RISK INVENTORY + SPIKE PLAN

**Pattern: Reviewer** — score against rubric, not free-form assessment.
**Output:** `.claude/plan/04-risk-inventory.md`
**Gate:** Read `references/phase-completion-gates.md` → Section "Phase 4 Gate"

### Instructions

Read `references/risk-assessment-rubric.md`.

Score each risk area against the rubric dimensions:
- **Unknownness** (1–3): How well do we understand the solution path?
- **Blast Radius** (1–3): If this fails late, how much work is wasted?
- **Reversibility** (1–3): How hard to recover if the approach is wrong?

Risk Score = Unknownness × Blast Radius × Reversibility (max 27)

Risk areas to evaluate:
1. TUI (Ink/React → Ratatui) — interactive rendering model differences
2. Streaming API handling — SSE parsing + backpressure in Tokio
3. Plugin system — JS plugins via stdio bridge
4. MCP transport — stdio + HTTP SSE in Rust
5. OAuth flow — browser launch + local HTTP redirect
6. Platform Keychain — macOS/Windows/Linux secure storage
7. Terminal input handling — crossterm edge cases (vim mode, chords)
8. Large output rendering — diff display, syntax highlight performance
9. Session compaction — context-window-aware truncation logic
10. Multi-agent coordination — UDS socket IPC in Rust

For each risk with score ≥ 12 (HIGH):
- Define a Spike: what minimal prototype would validate the approach?
- Define Spike acceptance criteria: what must the prototype demonstrate?
- Estimate Spike scope: can one agent complete it in a single session?

Write `.claude/plan/04-risk-inventory.md`.
Then run the Reviewer gate.

---

## PHASE 5 — STRATEGY DECISION: Big Bang vs Strangler Fig

**Pattern: Inversion + Reviewer**
**Output:** `.claude/plan/05-strategy-decision.md`
**Gate:** Read `references/phase-completion-gates.md` → Section "Phase 5 Gate"

### Instructions

DO NOT make this decision without first reading:
- `.claude/plan/01-known-context.md` (constraints, definition of complete)
- `.claude/plan/03-behavior-contracts.md` (what must be preserved)
- `.claude/plan/04-risk-inventory.md` (high-risk areas)

Read `references/strategy-decision-framework.md` for the full evaluation criteria.

Then present the user with a structured comparison:

```
BIG BANG:
  Pros given our context: [list]
  Cons given our context: [list]
  Works well because: [specific to THIS project]
  Risks: [specific HIGH risks from Phase 4 that affect this choice]

STRANGLER FIG:
  Pros given our context: [list]
  Cons given our context: [list]
  Works well because: [specific to THIS project]
  Risks: [specific to this approach]

HYBRID (phased cutover):
  Description: [how this would work specifically]
  Pros/Cons: [list]
```

Ask the user: "Which strategy do you choose, and what is your primary reason?"

DO NOT proceed until user gives a clear answer.

Write `.claude/plan/05-strategy-decision.md` with:
- The chosen strategy
- The user's stated reason
- The implications for Phase 6 (build order changes based on strategy)

Then run the Reviewer gate.

---

## PHASE 6 — DEPENDENCY GRAPH + BUILD ORDER

**Pattern: Pipeline + Subagent Exploration**
**Output:** `.claude/plan/06-dependency-graph.md`
**Gate:** Read `references/phase-completion-gates.md` → Section "Phase 6 Gate"

### Instructions

The build order depends on the strategy chosen in Phase 5. Read `.claude/plan/05-strategy-decision.md` first.

Spawn an Explore subagent (thoroughness: "very thorough") to map the import/dependency relationships between the 20 planned Rust crates:

```
cc-core, cc-config, cc-auth, cc-api, cc-session, cc-permissions,
cc-tools, cc-mcp, cc-hooks, cc-git, cc-memory, cc-tasks, cc-agent,
cc-skills, cc-plugins, cc-commands, cc-query, cc-tui, cc-bridge, cc-analytics
```

For each crate pair (A, B): does A depend on B? (A imports types/functions from B?)

Produce:
1. **Dependency matrix** — which crates depend on which
2. **Build layers** — crates with no dependencies (Layer 0), crates depending only on Layer 0 (Layer 1), etc.
3. **Parallelization map** — which crates in each layer can be built simultaneously
4. **Critical path** — the longest sequential chain that determines minimum total time

Format the dependency graph as:
```
Layer 0 (no dependencies, build first):
  - cc-core

Layer 1 (depends only on Layer 0):
  - cc-config (depends on: cc-core)
  - cc-auth (depends on: cc-core)
  ...

Layer 2:
  ...

Critical path: cc-core → cc-api → cc-query → cc-tui (N layers deep)
```

Write `.claude/plan/06-dependency-graph.md`.
Then run the Reviewer gate.

---

## PHASE 7 — MILESTONES + GATE CRITERIA

**Pattern: Generator** — fill template with data from previous phases.
**Output:** `.claude/plan/07-milestones.md`
**Gate:** Read `references/phase-completion-gates.md` → Section "Phase 7 Gate"

### Instructions

Read ALL previous phase outputs:
- `.claude/plan/01-known-context.md` (definition of complete, constraints)
- `.claude/plan/04-risk-inventory.md` (high risks → Spike milestones)
- `.claude/plan/05-strategy-decision.md` (strategy → milestone structure)
- `.claude/plan/06-dependency-graph.md` (build layers → milestone groupings)

Define milestones by grouping build layers into deliverable phases.
Each milestone MUST have:

```markdown
## Milestone N: [Name]

**Crates included:** [list from dependency graph layers]
**Deliverable:** [what can be demonstrated when this milestone is complete]

**Entry criteria:** (what must be true to START this milestone)
- [ ] [specific, checkable condition]

**Exit criteria:** (what must be true to call this milestone DONE)
- [ ] [specific, checkable condition — not "feature works" but "cargo test passes" or "claude --help output matches original"]

**Spike validation:** (if any HIGH-risk items from Phase 4 are in this milestone)
- [ ] [Spike acceptance criteria from Phase 4]

**Parallel tracks:** (which crates in this milestone can be built simultaneously)
- Track A: [crates]
- Track B: [crates]
```

Gate criteria must be measurable. REJECT vague criteria like:
- ❌ "TUI works correctly"
- ✅ "claude --help renders correctly in 80-column terminal on macOS, Linux, Windows"
- ❌ "streaming feels smooth"
- ✅ "first token latency < 500ms on M1 Mac, measured by `time` against Claude API"

Write `.claude/plan/07-milestones.md`.
Then run the Reviewer gate.

---

## PHASE 8 — SYNTHESIS: Generate Planning Document

**Pattern: Generator** — template-driven, no free-form invention.
**Output:** `RUST_REWRITE_PLAN_V2.md`

### HARD GATE — DO NOT PROCEED WITHOUT THIS CHECK

Before writing anything, verify ALL of the following files exist:
```
.claude/plan/01-known-context.md
.claude/plan/02-compatibility-contracts.md
.claude/plan/03-behavior-contracts.md
.claude/plan/04-risk-inventory.md
.claude/plan/05-strategy-decision.md
.claude/plan/06-dependency-graph.md
.claude/plan/07-milestones.md
```

If ANY file is missing: STOP. Report which files are missing. Do not write the synthesis document.

### Instructions

Read `assets/planning-document-template.md`.
Fill every section of the template using ONLY the content from Phase 1–7 output files.

Rules for synthesis:
- DO NOT invent new content not present in Phase outputs
- DO NOT add implementation details (Rust code, type definitions, crate internals)
- DO NOT add technology choices not already recorded in Phase outputs
- Every claim must trace back to a Phase output file

Update `.claude/plan/state.md` to mark Phase 8 as complete.

---

## STATE FILE FORMAT

`.claude/plan/state.md`:
```markdown
# Rust Rewrite Plan — Phase State

| Phase | Status | Output File | Last Updated |
|-------|--------|-------------|--------------|
| 1 - Known Context | pending/in-progress/complete | .claude/plan/01-known-context.md | - |
| 2 - Compatibility Contracts | pending | .claude/plan/02-compatibility-contracts.md | - |
| 3 - Behavior Contracts | pending | .claude/plan/03-behavior-contracts.md | - |
| 4 - Risk Inventory | pending | .claude/plan/04-risk-inventory.md | - |
| 5 - Strategy Decision | pending | .claude/plan/05-strategy-decision.md | - |
| 6 - Dependency Graph | pending | .claude/plan/06-dependency-graph.md | - |
| 7 - Milestones | pending | .claude/plan/07-milestones.md | - |
| 8 - Synthesis | pending | RUST_REWRITE_PLAN_V2.md | - |
```
