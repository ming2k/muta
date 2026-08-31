# Pattern: `docs/dev/testing.md`

Intent: help contributors run, interpret, and extend automated test suites to
verify implementation correctness programmatically.

---

## Should Include

- **Scope**: Test suites covered (unit, integration, component, API, E2E).
- **Test Model & Channels**: Breakdown of testing frameworks and execution tiers.
- **Run Commands**: Full suite, single test, verbose logging, and debug modes.
- **Static Analysis & Type Checking**: Linter, formatter, and type-checker commands.
- **Coverage Targets**: Coverage metrics and reporting commands.
- **Failure Triage**: Symptom-to-subsystem diagnostic matrix.
- **Adding a Test**: Guidelines for naming, assertions, and test isolation.

---

## Should Not Include

- Manual human onboarding procedures (belongs in `acceptance.md`).
- Visual scenario inspection instructions (belongs in `experience_scenarios.md`).
- Full development environment setup (belongs in `setup.md`).

---

## Recommended Template

```markdown
# Testing

This document details how to execute and extend the automated test suite.

---

## Scope & Test Model

- **Unit Tests**: Fast, in-memory isolation tests.
- **Integration Tests**: Service boundary and database tests.
- **Static Analysis**: Linting and strict type checking.

---

## Run Commands

```bash
# Run full test suite
npm test

# Run single test file
npm test -- path/to/test.spec.ts

# Run static checks
npm run lint && npm run typecheck
```

---

## Failure Triage Matrix

| Symptom | Likely Cause | First Inspection |
|---------|--------------|------------------|
| Connection refused | Local service container not running | Check Docker daemon |
| Type mismatch | Outdated schema definitions | Run codegen |

---

## Adding New Tests

1. Place unit tests alongside source files or in `tests/`.
2. Follow deterministic naming: `<module>.test.ts`.
3. Assert both nominal outcomes and error branches.
```
