# Risk Assessment Rubric

Score each risk on 3 dimensions. Risk Score = U × B × R (max 27).

---

## Dimension 1: Unknownness (U)

How well do we understand the solution path in Rust?

| Score | Meaning |
|-------|---------|
| 1 | Clear solution path — well-known Rust pattern, existing crate handles it |
| 2 | Partial clarity — general approach known, but edge cases unclear |
| 3 | High unknownness — no clear Rust equivalent, needs original design |

---

## Dimension 2: Blast Radius (B)

If this approach turns out to be wrong, how much completed work is wasted?

| Score | Meaning |
|-------|---------|
| 1 | Isolated — only 1 crate affected, easy to swap out |
| 2 | Medium — affects 2–5 crates or a cross-cutting interface |
| 3 | Wide — affects core architecture, requires redesign of multiple systems |

---

## Dimension 3: Reversibility (R)

How hard is it to recover if the chosen approach is wrong?

| Score | Meaning |
|-------|---------|
| 1 | Easy — can swap implementation behind an interface with minimal churn |
| 2 | Medium — requires refactoring callers, but scoped |
| 3 | Hard — architectural decision baked into many places, very costly to change |

---

## Thresholds

| Score Range | Risk Level | Required Action |
|-------------|------------|-----------------|
| 1–6 | LOW | Document and proceed |
| 7–11 | MEDIUM | Document mitigation strategy |
| 12–27 | HIGH | MUST define a Spike with acceptance criteria before planning this area |

---

## Spike Definition Template (for HIGH risks)

```
Risk: [name]
Score: [U × B × R = total]

Spike Goal:
  What specific question does the Spike answer?
  (Not "does Ratatui work" but "can Ratatui render a scrollable diff with ANSI colors
   in a 120-column terminal while receiving streaming input?")

Spike Prototype:
  What is the minimal implementation?
  (A standalone binary that demonstrates ONLY this capability)

Acceptance Criteria:
  - [ ] [specific, runnable test]
  - [ ] [specific, runnable test]

Scope Assessment:
  Can one agent complete this in a single session? [yes/no/uncertain]
  If no: describe how to split it.
```
