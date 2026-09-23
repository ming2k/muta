# Repository Governance

Top-level governance charter, decision hierarchy, review gates, and documentation standards for `muta`.

| Section | Purpose |
|---------|---------|
| [Documentation Governance](documentation/core/index.md) | Zero-Vendoring Architecture (Protocol v6.0.0): Layered invariants (deterministic AST [INV-LINT-*] & cognitive agent protocols [INV-AGENT-*]), flat knowledge topology, in-place frontmatter lifecycle, and declarative repository contracts |

## Core Meta-Governance
- [Taxonomy](documentation/core/taxonomy.md): 4D spatial coordinate tensor (Temperature x Lifecycle x Audience x Mode) with flat topology and frontmatter lifecycle.
- [Invariants](documentation/core/invariants.md): Codified constitution of layered system invariants: machine linter (`[INV-LINT-*]`) and cognitive agent rules (`[INV-AGENT-*]`).
- [Workflow](documentation/core/workflow.md): Code-to-doc trigger matrix, PR review gates, standard intake SOP, and zero-vendoring adoption.
- [Style Guide](documentation/core/style.md): Technical voice, structural syntax, link contracts.
- [Repository Contracts](documentation/contracts.md): Active profiles, declarative contract schema (`.docgov.yml`), and directory layout bindings.

## Active Domain Profiles
- **Architecture**: [Architecture Profile](documentation/profiles/architecture/index.md) (`adr.md`, `living-snapshot.md`, `rfc.md`).
- **Validation**: [Validation Profile](documentation/profiles/validation/index.md) (`acceptance.md`, `testing.md`).

## Governance Structure & Decision Hierarchy

1. **Architecture Decision Records (`docs/adr/`)** — Durable, immutable architectural records for significant structural choices, evolved in-place via Frontmatter metadata.
2. **Repository Governance (`docs/governance/`)** — Top-level charters, standards, intake filters, and documentation governance.
3. **Internal Contributor Guides (`docs/dev/`)** — Firewall-protected guides for local builds, testing isolation, and release procedures.
4. **User-Facing Surfaces (`docs/tutorials/`, `docs/how-to/`, `docs/reference/`, `docs/explanation/`)** — External documentation organized by user learning mode.
