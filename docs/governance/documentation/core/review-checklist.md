# Review Checklist

Use this checklist before approving documentation changes.

## Structure & Routing

- File passed through the correct gate in [Routing](routing.md); the gates
  are a priority-ordered cascade, not a free choice.
- Active domain profiles obey their respective boundaries:
  - Validation: `acceptance.md` verifies authentic paths without backdoors;
    `experience_scenarios.md` uses direct entry without faking live experience;
    `testing.md` covers programmatic implementation.
  - Architecture: ADRs are immutable once accepted.
  - Operations: Postmortems maintain evidence chains and feed durable layers.
- Explanation docs stay conceptual: no source coordinates, struct layouts, API
  signatures, or fenced source blocks.
- User-facing and contributor content are not mixed; `docs/dev/` is the
  firewall between them.
- Root files are used only for their fixed purpose, not as a routing escape hatch.
- New documentation directories include an `index.md`.
- Repository-specific rules are declared as contracts in [Repository Contracts](../contracts.md).

## Accuracy

- Statements match the current code and system behavior.
- Symbol names, config keys, CLI flags, schema fields, and protocol names are exact.
- Outdated information has been removed.
- Intra-repository links resolve without broken relative paths.

## Writing Conventions

- Document has exactly one `H1`.
- Heading levels are not skipped (`H1` → `H2` → `H3`).
- Code blocks specify language tags.
- Inline code identifiers use backticks.
- Link text is descriptive (no generic "here" or "link").
- Terminology follows the project glossary when one exists.

## Portability & Guardrails

- Core governance files avoid project names and single-repository assumptions.
- AI-generated policy modifications are reviewed and approved by a human maintainer.
