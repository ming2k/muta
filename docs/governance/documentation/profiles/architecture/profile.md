# Architecture Profile

This profile defines the Architecture Decision Record (ADR) lifecycle and
decision governance for repositories with long-lived architectural invariants.

---

## The ADR Philosophy

An Architectural Decision Record (ADR) captures an important architectural
decision made along with its context, considered options, and trade-offs.

### Key Invariants
1. **Immutable Historical Record**: Once an ADR is marked `Accepted`, it is
   never edited to reflect a new state. If a decision changes, write a new ADR
   that supersedes the old one.
2. **First Gate in Routing**: Decisions and their rationale belong in
   `docs/adr/` (Gate 1 - Time), completely firewalled from tutorials,
   reference manuals, and developer how-to guides.
3. **Traceability**: Code comments and explanation pages link to ADR numbers
   when explaining why a counterintuitive design constraint exists.

---

## ADR Lifecycle

```text
       [ Proposed ] ───► [ Accepted ] ───► [ Deprecated ]
             │                  │
             │                  └───► [ Superseded by NNNN ]
             ▼
       [ Rejected ]
```

- **Proposed**: Open for stakeholder review and feedback.
- **Accepted**: Approved and binding on the repository.
- **Rejected**: Evaluated but not adopted. Kept for historical context.
- **Deprecated**: No longer relevant; no replacement.
- **Superseded**: Replaced by a subsequent accepted ADR.

---

## Structure & Numbering

- ADRs live under `docs/adr/NNNN-<slug>.md` where `NNNN` is a zero-padded integer (e.g., `0001-modular-governance.md`).
- A registry index must exist at `docs/adr/index.md`.

---

## Patterns in this Profile

- [ADR Pattern](adr.pattern.md)
