# Strategy Decision Framework: Big Bang vs Strangler Fig

Use this framework in Phase 5. Evaluate BOTH options before asking the user to choose.

---

## Option A: Big Bang

**Definition:** Build the complete Rust version in parallel with the TypeScript version.
Ship the Rust version as a full replacement when it reaches feature parity.

**Evaluate these factors:**

| Factor | Favors Big Bang if... |
|--------|----------------------|
| Codebase coupling | TypeScript modules are highly interdependent (hard to replace one at a time) |
| Interface stability | Internal interfaces change frequently during the rewrite |
| Team velocity | Faster to build fresh without maintaining compatibility shims |
| Risk tolerance | Acceptable to have a long period with no shippable Rust version |
| Test coverage | Original has low test coverage (hard to verify incremental replacements) |
| Architecture delta | Rust design differs significantly from TypeScript (not a 1:1 port) |

**Known risks for this project:**
- Long period with no deliverable → morale and momentum risk
- Late integration failures → all modules must work together before anything ships
- Harder to validate behavior parity (no side-by-side comparison during development)

---

## Option B: Strangler Fig

**Definition:** Replace TypeScript modules with Rust equivalents one at a time.
The system runs in a hybrid state during transition, with a compatibility layer.

**Evaluate these factors:**

| Factor | Favors Strangler Fig if... |
|--------|---------------------------|
| Deliverable pressure | Need to ship something working at regular intervals |
| Risk management | Want to validate each Rust module independently before proceeding |
| Compatibility shim cost | The boundary between old and new is cleanly definable |
| Module independence | Individual modules can be replaced without breaking the rest |
| Test strategy | Can run TypeScript and Rust side-by-side to verify behavior |

**Known risks for this project:**
- Compatibility shim between Rust ↔ TypeScript (subprocess IPC) adds complexity
- Two systems to maintain simultaneously
- Some modules may be too tightly coupled to extract cleanly

---

## Option C: Hybrid (Phased Cutover)

**Definition:** Big Bang within each phase, but phases are independently shippable.
Phase 1 ships a minimal Rust version (e.g., basic conversation, no tools).
Phase 2 adds tool execution. Phase 3 adds TUI. Etc.

**When this makes sense:**
- Modules within a phase are tightly coupled (Big Bang within phase)
- Phases have clean interfaces between them (Strangler Fig across phases)
- Early phases can be shipped to users for validation

---

## Decision Output Format

```markdown
## Strategy Decision

**Chosen strategy:** [Big Bang / Strangler Fig / Hybrid]

**Primary reason:** [one sentence — must reference specific project context, not generic reasoning]

**Supporting factors:**
- [factor from framework that supports this choice]
- [factor from framework that supports this choice]

**Acknowledged tradeoffs:**
- [what we're giving up with this choice]

**Impact on build order:**
- [how this changes Phase 6 — e.g., "Strangler Fig means Layer 0 must have clean FFI boundaries"]
```
