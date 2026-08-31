# Pattern: `docs/dev/acceptance.md`

Intent: demonstrate how an authentic user starts from the normal entry point,
without shortcuts or test backdoors, and completes core user journeys end-to-end.

---

## Should Include

- **Scope**: Which user journeys are validated and which are explicitly out of scope.
- **Prerequisites**: Required environment, external credentials, and hardware/network setup.
- **Launch & Entry**: Normal installation and launch commands, authentic login or entry URLs.
- **Core User Journeys**: Numbered, step-by-step human operations.
- **Verification per Step**: Observable results and feedback expected after each action.
- **Pass / Fail Criteria**: Explicit success thresholds and failure conditions.
- **Final Checklist**: Itemized checklist for formal acceptance sign-off.

---

## Should Not Include

- Shortcut environment variables or backdoors to bypass onboarding.
- Manual database injection or synthetic fixture substitution.
- Headless unit, integration, or API contract test instructions.
- Static analysis, linting, or test coverage figures.

---

## Recommended Template

```markdown
# Acceptance

This document defines the real, end-to-end acceptance procedure for [Product Name].

It answers: **Can an authentic user launch, configure, and complete core journeys through standard product interfaces?**

---

## Scope & Prerequisites

- **Scope**: Core workflows validated in this release.
- **Prerequisites**: Minimum environment, credentials, or accounts required.

---

## Launch & User Entry Point

1. Start the application using standard commands:
   ```bash
   npm start
   ```
2. Navigate to `http://localhost:3000` in a standard browser.

---

## Core User Journeys

### Journey 1: [Journey Name]

#### Steps
1. Step 1: [Action taken by user]
2. Step 2: [Action taken by user]

#### Expected Outcome
- Observable state rendered on screen.

#### Pass / Fail Criteria
- **Pass**: Expected output appears and meets criteria.
- **Fail**: Error screen or blocked workflow.

---

## Final Acceptance Checklist

- [ ] Core journey executes from cold start without manual intervention.
- [ ] No test-only flags or synthetic state injections used.
- [ ] Final result matches expected user outcome.
```
