# 0073. Flat coding-focused workspace

- **Status:** Accepted
- **Date:** 2026-07-23

## Context

ADR-0064 introduced a product-family workspace layout — `apps/code/`,
`apps/editor/`, `apps/quant/`, `crates/platform/`, `crates/providers/` — so
three sibling products (coding, editor, quant) could share one repository
while keeping ownership visible. That grouping only earned its keep when more
than one product competed for the same shared platform packages.

The editor and quant products have been removed. The repository now ships a
single product, the `neenee-code` coding agent. With one product, the
`apps/<product>/` family tier, the `crates/platform/` vs `crates/providers/`
split, and the depth they add all describe a distinction that no longer
exists. Every application-specific support package (`neenee-tui`,
`neenee-tui-view`) has exactly one consumer, and the platform/provider
packages are the only shared substrate.

Removing the products also dropped their dependencies: the `iris`/optics GUI
stack and the `longport` trading SDK are gone from the workspace manifest and
lockfile. ADR-0062 (LongPort quant adapter) and ADR-0063 (intelligence
workbench and expert council) describe code that no longer exists.

## Decision

Collapse the workspace to a single flat `crates/` directory containing every
member:

- Move `apps/code/*`, `crates/platform/*`, and `crates/providers/*` directly
  under `crates/`.
- Delete `apps/editor/` and `apps/quant/` (the `neenee-editor`,
  `neenee-quant`, `neenee-quant-gui`, and `neenee-intelligence` packages).
- Express workspace membership with one glob, `members = ["crates/*"]`, and
  keep `neenee-code` as the default member.
- Drop the now-unused `iris` and `longport` workspace dependencies.
- Preserve Cargo package names and the dependency DAG. Directory containment
  is location only; it creates no reverse dependency and grants no capability.

Package names are unchanged, so the `cargo -p <name>` selector is unaffected.
Select packages by name rather than by directory location.

## Alternatives considered

### Keep the product-family layout from ADR-0064

Rejected. The intermediate directories exist to distinguish products and
shared substrate. With one product they add path depth and ownership rules
that describe a distinction that is no longer present.

### Keep `apps/` for the binary and `crates/` for libraries

Rejected. It preserves a binary/library split but reintroduces a two-root
layout whose only justification was separating multiple application families.
A single product does not need it, and a flat `crates/` keeps every member at
the same depth.

### One repository per package

Rejected. The packages still share contracts, change together atomically, and
have not stabilized interfaces for independent versioning.

## Consequences

- Every workspace member is one directory level deep under `crates/`.
- Source paths and documentation were updated mechanically; package names and
  the build graph are unchanged.
- `iris`/optics and `longport` are no longer in `Cargo.lock`, so builds and CI
  no longer fetch or exclude them.
- ADR-0062, ADR-0063, and ADR-0064 are superseded. Their text is retained as
  historical record.
- A second product can be reintroduced later; doing so would warrant a new
  layout decision rather than reverting this one.

## References

- [ADR-0062](0062-longport-openapi-quant-adapter.md) — superseded (quant removed).
- [ADR-0063](0063-intelligence-workbench-and-expert-council.md) — superseded (quant removed).
- [ADR-0064](0064-product-family-workspace-layout.md) — superseded (the product-family layout this flattens).
- [Workspace layout](../dev/workspace-layout.md)
