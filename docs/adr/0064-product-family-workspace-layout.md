# 0064. Product-family workspace layout

- **Status:** Accepted
- **Date:** 2026-07-14

## Context

The workspace contains three application products and a set of shared platform
and provider packages. Keeping every package directly under `crates/` makes
application ownership invisible: `neenee-code`, `neenee-editor`, and the quant
application appear beside contract, transport, persistence, and protocol
implementation crates even though they have different responsibilities.

The flat directory also makes the number of Cargo packages look like a number
of independently released projects. A Cargo package boundary exists for
dependency direction, test isolation, or compile isolation; it does not imply
an independent repository or release train.

The products still benefit from atomic changes against shared platform code.
The platform interfaces and application release cycles are not yet stable
enough to replace local dependencies with independently versioned artifacts.

## Decision

Keep one Git repository, one Cargo workspace, and one lockfile. Organize
workspace members by ownership:

- Put coding-product packages under `apps/code/`.
- Put the editor package under `apps/editor/`.
- Put quantitative-product packages under `apps/quant/`.
- Put reusable agent, session, persistence, tool, skill, connector, and
  contract packages under `crates/platform/`.
- Put provider integration and protocol SDK packages under
  `crates/providers/`.

Preserve Cargo package names and the dependency DAG. Directory containment
expresses ownership only; it creates no reverse dependency and grants no
capability. Keep `neenee-code` as the default workspace member for root Cargo
commands because it is the default development command, not because it owns
the sibling products.

Use grouped workspace member globs instead of enumerating every package. Keep
application-specific support packages with their application until a second
independent consumer justifies promotion to the shared package groups.

Repository extraction remains a separate decision. Split an application into
another repository only when ownership, access control, or release cadence
diverges and the shared platform can be consumed through a stable versioned
interface.

## Alternatives considered

### Keep the flat `crates/` directory

Rejected. It minimizes path depth but hides the distinction between product
ownership and reusable platform boundaries, which is the source of the current
navigation and topology confusion.

### Create one repository per application immediately

Rejected. The coding and quant products still use local platform packages and
change with them atomically. Immediate extraction would replace simple Cargo
paths with cross-repository version coordination before the interfaces have
stabilized.

### Create nested Cargo workspaces per product

Rejected. Multiple lockfiles and workspace inheritance roots would complicate
dependency resolution and shared checks without providing a current ownership
or release benefit.

### Merge product-support crates into each application binary

Rejected. The existing crate boundaries enforce useful dependency directions
and support focused tests. Directory grouping solves the ownership problem
without discarding those boundaries.

## Consequences

- Product ownership is visible from the filesystem without changing package
  names or runtime behavior.
- Root Cargo commands retain one dependency resolution and build cache.
- Application-specific support crates no longer look like generic shared
  libraries merely because they are Cargo packages.
- Source paths and contributor documentation require a one-time mechanical
  update.
- The directory hierarchy becomes deeper, so contributors should select
  packages with `cargo -p` rather than relying on package locations.
- Independent application versions and repositories can be introduced later
  without another conceptual topology change.

## References

- [ADR-0035](0035-application-layer-split.md)
- [ADR-0045](0045-extract-neenee-tui-view.md)
- [ADR-0060](0060-skills-and-mcp-extension-boundaries.md)
- [Workspace layout](../dev/workspace-layout.md)
