# Validation Profile

This profile defines the three-tier product validation model for engineering
repositories with interactive deliverables, user journeys, or complex service
boundaries.

```text
       +-------------------------------------------------------+
       |                     acceptance.md                     |
       |  Verifies Journeys: "How users get there from entry"  |
       +-------------------------------------------------------+
                                  |
                                  v
       +-------------------------------------------------------+
       |                experience_scenarios.md                |
       |    Verifies Experiences: "What users experience there" |
       +-------------------------------------------------------+
                                  |
                                  v
       +-------------------------------------------------------+
       |                      testing.md                       |
       |  Verifies Implementation: "Why the code is trusted"   |
       +-------------------------------------------------------+
```

---

## The Three-Tier Boundaries

| Document | Validation Target | Core Question | Concise Maxim |
|----------|-------------------|---------------|---------------|
| `docs/dev/acceptance.md` | Complete user journeys | Can users reach goals through normal paths? | **Acceptance validates how users get there.** |
| `docs/dev/experience_scenarios.md` | User-visible experiences | Is the experience correct once in this state? | **Experience scenarios validate what users experience there.** |
| `docs/dev/testing.md` | Code & system implementation | Does the code function as specified? | **Testing validates why the implementation can be trusted.** |

---

## Core Invariants

### 1. `acceptance.md` — Real User Journeys
- **Golden Rule**: Acceptance must mirror real human behavior without shortcuts.
- **Forbidden**: Setting environment variables to skip onboarding, manual DB state injection, calling internal test endpoints, or using debug routes to jump over mandatory steps.
- **Scope**: Cold-start prerequisites, normal launch, real user entry point, step-by-step journey, observable outputs, explicit pass/fail criteria, final acceptance checklist.

### 2. `experience_scenarios.md` — User-Visible Experience Scenarios
- **Golden Rule**: **"Skip the journey, not the experience."**
- **Controlled Entry**: Direct entry via environment variables (`APP_SCENARIO=completed-run`), scenario keys, query params, debug routes, or seed fixtures is encouraged.
- **Authentic UX Invariant**: Once entered, the target state must render live, interactive components (buttons, tables, error banners, follow-up actions) without substituting mock fake UI.

### 3. `testing.md` — Implementation Correctness
- **Golden Rule**: Programmatic verification of algorithms, interfaces, and system internals without requiring UI interaction.
- **Scope**: Unit, integration, component, API contract, and automated E2E test suites, linters, type-checkers, and CI matrices.

---

## Non-Substitution Principle

The three tiers are complementary, not interchangeable:
```text
Implementation Correctness (testing.md)
              +
Experience Correctness (experience_scenarios.md)
              +
Journey Correctness (acceptance.md)
```

- Automated tests passing does not guarantee journey usability.
- A functional scenario shortcut does not prove cold-start reachability.
- A passing happy path does not prove resilience across internal failure branches.

---

## Patterns in this Profile

- [Acceptance Pattern](acceptance.pattern.md)
- [Experience Scenarios Pattern](scenarios.pattern.md)
- [Testing Pattern](testing.pattern.md)
