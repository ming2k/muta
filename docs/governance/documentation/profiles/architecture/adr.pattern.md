# Pattern: `docs/adr/NNNN-<slug>.md`

Intent: record a durable architectural decision and its historical context for the lifetime of the project.

---

## Directory Setup

Create `docs/adr/` with an `index.md` and a `template.md` before writing the first record:
- `docs/adr/index.md` — The table of all ADRs with their status and date.
- `docs/adr/template.md` — The baseline starting template for new proposals.

---

## Recommended Template

```markdown
# NNNN. Title

- Status: Proposed | Accepted | Rejected | Deprecated | Superseded by [NNNN](NNNN-slug.md)
- Date: YYYY-MM-DD
- Deciders: List everyone involved in the decision
- Consulted: List everyone whose input was considered
- Informed: List everyone informed of the decision

## Context and Problem Statement

Describe the context and problem statement in a few sentences. What problem are we solving, and why does it matter now?

## Decision Drivers

- Driver 1: e.g., Modularity, maintainability, performance
- Driver 2: e.g., Compatibility with existing tooling

## Considered Options

- Option 1: Title of option 1
- Option 2: Title of option 2
- Option 3: Title of option 3

## Decision Outcome

Chosen option: "[Option 1]", because [justification].

### Positive Consequences

- Positive consequence 1
- Positive consequence 2

### Negative Consequences

- Negative consequence 1 (trade-off)
- Mitigation strategy

## Pros and Cons of the Options

### Option 1

- Good, because [argument a]
- Bad, because [argument b]

### Option 2

- Good, because [argument a]
- Bad, because [argument b]

## Links

- Related PRs or issues
- Related ADRs
```
