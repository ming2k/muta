# Standard Evolution & Intake SOP

This document defines the formal admission gates and classification rules for
proposing changes, extensions, and new patterns in `docs-governance`.

To maintain architectural integrity and prevent unstructured sprawl, all
contributions must pass through the **Four-Gate Intake Filter**.

---

## The Four-Gate Intake Filter

```text
                       [ New Standard Proposal ]
                                   │
                                   ▼
        ┌─────────────────────────────────────────────────────┐
        │ Gate 1: Universality Filter                         │
        │ Is this universally applicable to >=80% of software │
        │ engineering repositories across all languages?      │
        └──────────────┬───────────────────────────────┬──────┘
                      YES                             NO
                       │                               │
                       ▼                               ▼
        ┌──────────────────────────────┐ ┌───────────────────────────┐
        │ Gate 2: Protocol Orthogonality│ │ Gate 3: Domain Methodology│
        │ Does this alter meta-routing,│ │ Is this a self-contained, │
        │ style, or verification logic?│ │ structured engineering    │
        └──────┬────────────────┬──────┘ │ philosophy across docs?   │
              YES               NO       └──────┬─────────────┬───────┘
               │                 │             YES            NO
               ▼                 ▼              │              │
        ┌──────────────┐ ┌──────────────┐       │              ▼
        │ Tier 0: Core │ │ Tier 1: Core │       ▼      ┌───────────────┐
        │ Protocol     │ │ Pattern      │ ┌──────────┐ │ REJECT:       │
        │ (core/*.md)  │ │ (common-     │ │ Tier 2:  │ │ Project-      │
        │              │ │  patterns.md)│ │ Domain   │ │ Specific      │
        └──────────────┘ └──────────────┘ │ Profile  │ │ (Local-only)  │
                                          │ (profile)│ └───────────────┘
                                          └──────────┘
```

---

## Tier Classification Matrix

| Tier | Category | Criteria | Destination | Version Impact |
|------|----------|----------|-------------|----------------|
| **Tier 0** | **Core Protocol** | Fundamental rules defining content routing, voice/style invariants, checklist gates, or verification mechanics. | `core/*.md` | MAJOR or MINOR protocol version bump. |
| **Tier 1** | **Core Patterns** | Structural layout for a single recurring repository document (e.g., `setup.md`, `release.md`). | `core/common-patterns.md` | MINOR or PATCH protocol update. |
| **Tier 2** | **Domain Profile** | A cohesive, specialized engineering methodology spanning multiple documents or lifecycles (e.g., product validation, ADRs, postmortems). | `profiles/<domain>/` | MINOR protocol update; optional for adopters. |
| **Tier 3** | **Project-Specific** | Framework-specific guides, company-specific organizational charts, or local API design rules. | **REJECT from standard.** Kept in adopter's `docs/governance/<domain>.md`. |

---

## Proposal Checklist for Contributors

Before proposing new files to `docs-governance`:

1. **Classify the Tier**: Explicitly declare whether the proposal is Tier 0, Tier 1, or Tier 2.
2. **Justify Orthogonality**: Demonstrate that the new addition does not duplicate or contradict existing core rules.
3. **No Project Names**: Content must be 100% portable and free of repository-specific names or domain assumptions.
4. **Update Manifest & Hashes**: Recompute `.manifest.json` SHA-256 signatures via `./tools/update-hashes.sh`.
5. **Add Automated Tests**: Provide test coverage in `tests/test_tools.py` for any tooling changes.
