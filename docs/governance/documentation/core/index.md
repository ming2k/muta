# Documentation Governance

Rules for organizing, writing, reviewing, and updating project
documentation. This governance framework uses a **Micro-Core + Domain Profile
Extensions** architecture (Protocol v3.0.0).

The core protocol is universal and project-neutral. Adopting repositories
declare activated domain profiles in `contracts.md`.

## Use this guide

Before writing, modifying, or archiving documentation:

1. If adopting this guide in a repository, work through [Adoption](adoption.md).
2. Configure activated profiles and repository paths in [Repository Contracts](../contracts.md).
3. Route all content through the four gates in [Routing](routing.md).
4. Write content following [Writing Style](style-guide.md).
5. For baseline document types, use the patterns in [Common Patterns](common-patterns.md).
6. Check whether code changes require documentation updates with [Update Checklist](update-checklist.md).
7. Review pull requests against [Review Checklist](review-checklist.md).
8. For proposed changes to this governance standard itself, follow [Intake SOP](intake-sop.md).

## Core Directory Map

| Page | Purpose |
|------|---------|
| [Adoption](adoption.md) | One-time decisions and checklist for installing this governance |
| [Repository Contracts](../contracts.md) | Profile activation and path contracts for the adopting repository |
| [Routing](routing.md) | The 4-gate priority routing cascade |
| [Writing Style](style-guide.md) | Voice, headings, formatting, links, and cross-references |
| [Common Patterns](common-patterns.md) | Structural patterns for baseline project documents (README, setup, etc.) |
| [Update Checklist](update-checklist.md) | Code change-to-documentation update trigger matrix |
| [Review Checklist](review-checklist.md) | PR review gate checklist for maintainers |
| [Intake SOP](intake-sop.md) | Tier 0–3 admission gates and evolution policy for the standard |

## Available Domain Profiles

| Profile | Purpose | Directory |
|---------|---------|-----------|
| **Validation** | Three-tier product validation model (`acceptance.md`, `experience_scenarios.md`, `testing.md`) | `../profiles/validation/` |
| **Architecture** | Architecture Decision Records (ADRs) and decision lifecycles | `../profiles/architecture/` |
| **Operations** | Operational knowledge layers (Triage runbooks, postmortems) | `../profiles/operations/` |

## Maintainer rule

This directory is policy, not ordinary project documentation. AI assistants
may read it and suggest improvements, but must not directly modify it. A
human maintainer applies policy changes.
