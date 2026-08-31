# Common Document Patterns

This pattern library defines structural conventions for baseline repository
documents. For domain-specific methodologies (Validation, Architecture,
Operations), refer to the respective profiles in `../profiles/`.

---

## `README.md`

Intent: give a new reader the project identity, value proposition, and shortest successful start path.

Should include:
- What the project is.
- Who it is for and core features.
- The smallest useful install, build, run, or usage command.
- Links to full documentation, contribution guide, and license.

Should not include:
- Complete API or option reference.
- Full development environment setup.
- Long design rationale.

Recommended structure:
```text
# Project Name
## What It Is
## Quick Start
## Documentation
## Contributing
## License
```

---

## `CONTRIBUTING.md`

Intent: help contributors understand the expected workflow before opening issues or pull requests.

Should include:
- Where contributor documentation lives (`docs/dev/`).
- Build, test, and verification expectations.
- Commit, PR, and documentation update expectations.
- Code of conduct and security reporting links.

Should not include:
- Full development setup instructions (link to `docs/dev/setup.md`).
- Full test matrices.

---

## `AGENTS.md` (Repository Root)

Intent: provide global guidance, architectural constraints, and workflow rules to AI coding assistants operating in the repository.

Should include:
- Repository mission and key architectural invariants.
- Toolchain commands for building, testing, and linting.
- Documentation routing policies (referencing `docs/governance/`).
- Critical safety guardrails and sensitive boundaries.

Should not include:
- Exhaustive API listings or ephemeral sprint tasks.

---

## `CHANGELOG.md`

Intent: record user-visible changes across releases.

Should include:
- An `Unreleased` section for pending changes.
- Released versions with ISO dates.
- User-visible additions, fixes, removals, and breaking behavior changes.
- Migration notes when configuration or APIs change.

Should not include:
- Internal refactors with no observable user effect.
- Full raw git commit logs.

---

## `SECURITY.md`

Intent: tell users and security researchers how to report vulnerabilities safely.

Should include:
- Supported versions or security support policy.
- Private reporting channel (email, security advisory).
- Expected disclosure timeline.

Should not include:
- Public exploit details.

---

## `docs/dev/setup.md`

Intent: help contributors create and maintain a working local development environment.

Should include:
- Required toolchains and system dependencies.
- Build and configure commands for contributor builds.
- Development flags (debug symbols, sanitizers, local overrides).
- Common setup failures and first triage checks.
- Links to testing and user-facing quick start.

Should not include:
- User-facing product introduction.
- Complete API option reference.

---

## `docs/dev/project-layout.md`

Intent: help contributors navigate the source tree, understand module boundaries, and place new files.

Should include:
- Source tree map.
- Module-to-purpose table.
- Rules for where new source, tests, examples, and docs go.

---

## `docs/dev/release.md`

Intent: help maintainers perform a deterministic, repeatable release.

Should include:
- Preconditions and branch state.
- Versioning and changelog update steps.
- Build and test gate verification.
- Artifact creation and publication steps.
- Post-release verification checks.

---

## `docs/explanation/design-philosophy.md`

Intent: articulate foundational architectural principles, technical invariants, and mental models of the system.

Should include:
- Core principles (orthogonality, descriptor paradigms, composition).
- System layer breakdown and boundary invariants.
- Structural mental model.

Should not include:
- Step-by-step how-to tutorials.
- Implementation code coordinates or function signatures.

---

## `docs/reference/api.md`

Intent: provide exact lookup for the public API symbols and contracts.

Should include:
- Public symbols, signatures, parameters, return types, and defaults.
- Ownership, lifetime, and error contracts.
- Links to task guides for usage examples.

---

## `docs/reference/glossary.md`

Intent: define canonical project terms so docs and code use consistent vocabulary.

Should include:
- Term.
- Short definition.
- Primary related documentation link.
- Avoided synonyms where ambiguity exists.
