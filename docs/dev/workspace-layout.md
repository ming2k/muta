# Workspace layout

The repository is one Cargo workspace organized by package ownership. Cargo
package boundaries express dependency and test boundaries; directories express
which product or shared subsystem owns each package.

## Directory map

```text
apps/
  code/
    neenee-code/
    neenee-tui/
    neenee-tui-view/
  editor/
    neenee-editor/
  quant/
    neenee-quant/
    neenee-intelligence/
    neenee-quant-gui/

crates/
  platform/
    neenee-core/
    neenee-store/
    neenee-auth/
    neenee-tools/
    neenee-skills/
    neenee-mcp/
    neenee-agent/
    neenee-session/
  providers/
    neenee-ai-sdk-core/
    neenee-ai-sdk-openai/
    neenee-ai-sdk-anthropic/
    neenee-ai-sdk-google/
    neenee-providers/
```

## Ownership rules

- Put a product binary and packages used only by that product under its
  `apps/<product>/` family.
- Put a package under `crates/platform/` only when multiple application
  families consume its contracts or behavior.
- Put model-provider protocols and their common facade under
  `crates/providers/`.
- Do not infer a dependency from directory containment. Cargo manifests remain
  the authoritative dependency graph.
- Promote an application-owned package to `crates/` when a second independent
  application consumes it and the API is stable enough to share.

## Cargo commands

Package names do not change with their directories. Continue to select focused
checks by package name:

```bash
cargo check -p neenee-store
cargo test -p neenee-agent
cargo check -p neenee-quant
```

Running Cargo without `--workspace` or `-p` selects `neenee-code`, the configured
default workspace member. Workspace-wide checks continue to use the shared
`Cargo.lock`, profiles, dependency versions, and lint policy.

## Repository boundaries

Keep an application in this repository while changes commonly cross its code
and shared platform packages. Consider a separate repository only when the
application has distinct ownership or access control, an independent release
cadence, and a stable versioned platform interface.

See [ADR-0064](../adr/0064-product-family-workspace-layout.md) for the decision
and alternatives.
