# Pattern: `docs/dev/experience_scenarios.md`

Intent: provide direct, reproducible mechanisms to enter specific user-visible
product states to validate visual presentation, live interactions, and edge
states without repeating preceding journeys.

---

## Should Include

- **Scope & Controlled Entry Overview**: Mechanisms used to load states (env vars, query parameters, debug routes, test fixtures).
- **Scenario Catalog**: Minimum coverage for nominal state, empty state, first-use state, completed state, loading state, error/failure state, expired session, and boundary inputs.
- **Per-Scenario Direct Entry Command**: Exact command or URL to enter the scenario.
- **Expected Presentation**: Visual layout, typography, charts, and banner feedback.
- **Live Interactive Checks**: Buttons to click, forms to submit, and follow-up flows.
- **Recovery & Exit Path**: How the reviewer exits or recovers from the scenario.

---

## Should Not Include

- Proof of journey reachability (belongs in `acceptance.md`).
- Headless unit or internal function assertions (belongs in `testing.md`).
- Mocked implementations that replace authentic UI components with static fakes.

---

## Recommended Template

```markdown
# Experience Scenarios

This document provides direct, controlled mechanisms to validate user-visible
product states.

Principle: **"Skip the journey, not the experience."**

---

## Scenario Catalog

### Scenario 1: [Scenario Name, e.g., Completed Job Result]

#### Direct Entry Command
```bash
APP_SCENARIO=completed-run npm start
```

#### Expected Presentation
- Dashboard renders completion banner with timestamp.
- Metrics summary table displays full dataset.

#### Interactive Checks
- Click "Export CSV" button → CSV file downloads.
- Click "Rerun Job" button → Confirmation modal appears.

#### Recovery & Exit Path
- Reset environment variable and restart standard process.
```
