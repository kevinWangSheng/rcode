# Phase Completion Gates (Reviewer Checklists)

Each gate must be checked before marking a phase complete.
FAIL on any single item = phase is NOT complete.

---

## Phase 1 Gate — Known Context

- [ ] All 5 questions answered (not skipped, not "TBD")
- [ ] At least one non-negotiable constraint is documented
- [ ] "Definition of complete" is specific enough to be testable
- [ ] Confirmed decisions are distinguished from open decisions
- [ ] Output file exists at `.claude/plan/01-known-context.md`

---

## Phase 2 Gate — Compatibility Contracts

- [ ] All 9 categories from the checklist are covered
- [ ] Each category references at least one actual source file (not invented)
- [ ] Each contract has a compatibility requirement level (MUST MATCH / READABLE / CAN CHANGE)
- [ ] API message format section quotes actual request/response structure
- [ ] MCP protocol section identifies the JSON-RPC version in use
- [ ] Output file exists at `.claude/plan/02-compatibility-contracts.md`

---

## Phase 3 Gate — Behavior Contracts

- [ ] All 5 behavior areas are covered (A through E)
- [ ] Each behavior cites at least one source file:line as evidence
- [ ] Each behavior is classified (MUST REPLICATE / ACCEPTABLE VARIANCE / INTENTIONALLY CHANGING)
- [ ] At least one "INTENTIONALLY CHANGING" behavior is documented (if none, that itself is a finding worth flagging)
- [ ] Subagent was used for exploration (not direct file reads only)
- [ ] Output file exists at `.claude/plan/03-behavior-contracts.md`

---

## Phase 4 Gate — Risk Inventory

- [ ] All 10 risk areas have been scored on all 3 dimensions
- [ ] Score calculation is shown (not just the final score)
- [ ] Every risk with score ≥ 12 has a Spike defined
- [ ] Every Spike has acceptance criteria (not just "prototype works")
- [ ] Every Spike scope is assessed (can one agent complete in one session?)
- [ ] Risk scores are not all the same (would indicate rubber-stamping)
- [ ] Output file exists at `.claude/plan/04-risk-inventory.md`

---

## Phase 5 Gate — Strategy Decision

- [ ] Both strategies (Big Bang AND Strangler Fig) are evaluated — not just the chosen one
- [ ] Hybrid option is at least mentioned
- [ ] The chosen strategy references specific findings from Phase 3 and Phase 4
- [ ] User explicitly stated their choice (not assumed from context)
- [ ] Implications for build order are noted
- [ ] Decision is NOT "Big Bang because it's simpler" without further justification
- [ ] Output file exists at `.claude/plan/05-strategy-decision.md`

---

## Phase 6 Gate — Dependency Graph

- [ ] All 20 crates are accounted for
- [ ] Each dependency is traced to an actual import relationship (not assumed)
- [ ] Build layers are explicitly numbered (Layer 0, Layer 1, ...)
- [ ] Critical path is identified and its length stated
- [ ] Parallel tracks are identified within each layer
- [ ] No circular dependencies (if found, they must be flagged and resolved)
- [ ] Output file exists at `.claude/plan/06-dependency-graph.md`

---

## Phase 7 Gate — Milestones

- [ ] Every milestone has entry AND exit criteria
- [ ] No exit criterion uses vague language ("works", "feels", "seems")
- [ ] Every HIGH-risk item from Phase 4 is addressed in a milestone's Spike validation
- [ ] Parallel tracks are identified within each milestone
- [ ] At least one milestone produces something demonstrable (not just internal types)
- [ ] First milestone is achievable without completing all 20 crates
- [ ] Output file exists at `.claude/plan/07-milestones.md`
