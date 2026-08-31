# Update Checklist

Use this checklist when code changes require accompanying documentation updates
in the same pull request.

| Code Change | Required Documentation Action | Routing Gate / Location |
|-------------|-------------------------------|-------------------------|
| Architectural decision made | Write a new ADR in `docs/adr/` (if Architecture profile active) | Gate 1 (Time) |
| Incident or latent operational risk discovered | Write a postmortem in `docs/dev/postmortems/` (if Operations profile active) | Gate 3 (Audience) |
| Core user journey, onboarding, or acceptance flow changed | Update `docs/dev/acceptance.md` (if Validation profile active) | Gate 3 (Audience) |
| User-visible scenario state or entry mechanism changed | Update `docs/dev/experience_scenarios.md` (if Validation profile active) | Gate 3 (Audience) |
| Automated test suites or test execution model changed | Update `docs/dev/testing.md` (if Validation profile active) | Gate 3 (Audience) |
| Local development environment or build setup changed | Update `docs/dev/setup.md` | Gate 3 (Audience) |
| Public API, CLI flag, config key, schema, or option changed | Update `docs/reference/` | Gate 4 (Lookup) |
| User-discoverable task or feature added | Add or update a how-to guide in `docs/how-to/` | Gate 4 (Doing) |
| Feature deprecated or removed | Mark deprecated in `docs/reference/`, update `docs/how-to/`, add migration note to `CHANGELOG.md` | Gate 4 / Root |
| Installation, build, or getting-started run steps changed | Update `README.md` quick start and getting-started tutorial | Root / Gate 4 |
| User-visible behavior changed | Add an "Unreleased" entry to `CHANGELOG.md` | Root |
| Pure internal refactor with no user-observable effect | No documentation change required | — |

## PR Requirement

If a pull request touches code with an active documentation contract, the PR
description must state whether documentation was updated or explain why an
update was not required.
