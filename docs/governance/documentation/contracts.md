# Repository Contracts

Reference data and bindings for documentation surfaces adopted by this
repository.

For adoption procedures, see [Adoption](core/adoption.md).

---

## 1. Activated Profiles

Declare the domain profiles active in this repository. `tools/sync.sh` and
`tools/verify.sh` use this list to assemble and verify documentation surfaces.

- [x] `core` (Mandatory: universal routing, writing style, checklists, patterns)
- [x] `validation` (Product validation: acceptance, experience scenarios, testing)
- [x] `architecture` (Architecture decision records: ADR lifecycle)
- [ ] `operations` (Operational knowledge layering: runbook triage, postmortems)

---

## 2. Directory Layout Bindings

| Surface | Path | Required | Purpose |
|---------|------|----------|---------|
| Core Governance | `docs/governance/documentation/` | Yes | Mirrored governance standard |
| Top-level Governance | `docs/governance/` | Yes | Repository charters and guidelines |
| Contributor Firewall | `docs/dev/` | Yes | Developer setup, testing, and procedures |
| Architecture Records | `docs/adr/` | If `architecture` active | Immutable Architecture Decision Records |
| Incident Records | `docs/dev/postmortems/` | If `operations` active | Post-incident analysis records |
| Root Entry | `README.md` | Yes | Pitch and shortest successful start path |
| Documentation Entry | `docs/index.md` | Yes | Documentation entry point |

---

## 3. Optional Document Contracts

| Contract | Active Condition | If Present | If Absent |
|----------|------------------|------------|-----------|
| `CHANGELOG.md` | Universal | User-visible changes update it in the same PR | Omit changelog checks from review |
| `CONTRIBUTING.md` | Universal | Contributor workflow links to `docs/dev/` | Add before accepting outside contributions |
| `docs/dev/acceptance.md` | Profile `validation` | User journey changes update it | Rely on developer testing guides |
| `docs/dev/experience_scenarios.md` | Profile `validation` | Visible state additions update scenario catalog | Rely on manual acceptance |
| `docs/dev/testing.md` | Profile `validation` | Test command or suite changes update it | Document testing in setup guide |
| `docs/adr/index.md` | Profile `architecture` | New ADRs registered upon acceptance | Create index before adding ADRs |
| `docs/reference/glossary.md` | Universal | New canonical terms update it | Keep definitions local to document |
