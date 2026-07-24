# Workspace layout

The repository is one Cargo workspace whose members live directly under
`crates/`. Cargo package boundaries express dependency and test boundaries; the
single flat directory keeps every member one level deep so package ownership
and the `cargo -p <name>` selector stay obvious.

## Directory map

```text
crates/
  neenee-cli/          # the interactive application binary (default workspace member)
  neenee-server/       # headless session host: one session served over WebSocket
  neenee-tui-engine/           # in-house grid + diff rendering engine
  neenee-transport/       # session harness, handlers, serve transport
  neenee-agent/         # orchestration: the turn/round loop, plus built-in tools (bash, read, grep, glob, webfetch, …)
  neenee-skills/        # skill discovery, registry, and tool adapters
  neenee-mcp/           # MCP stdio transport and tool publication
  neenee-persistence/         # durable state: session store, config, paths
  neenee-core/          # shared domain and wire contracts (no deps)
  neenee-llm-client/    # multi-protocol HTTP client (transport + openai/anthropic/google protocols)
  neenee-providers/     # channel registry, factory, discovery + provider facade + OAuth flows
```

## Ownership rules

- Every workspace member lives directly under `crates/`. There is no
  intermediate grouping directory; a package is selected by name, not by
  location.
- Put shared contracts in `neenee-core`, orchestration in `neenee-agent`, and
  the application binaries in `neenee-cli` / `neenee-server`. See
  [Crate layering](../explanation/crate-layering.md) for the dependency DAG.
- Do not infer a dependency from directory containment. Cargo manifests remain
  the authoritative dependency graph.
- The product focus is the coding agent. Application-specific support packages
  (`neenee-tui-engine`) stay alongside the platform and provider
  crates because they are consumed by the single application.

## Cargo commands

Package names match their directory names. Select focused checks by package
name:

```bash
cargo check -p neenee-persistence
cargo test -p neenee-agent
```

Running Cargo without `--workspace` or `-p` selects `neenee-cli`, the
configured default workspace member. Workspace-wide checks continue to use the
shared `Cargo.lock`, profiles, dependency versions, and lint policy.

## Repository boundaries

Keep a package in this repository while changes commonly cross it and shared
platform packages. Consider a separate repository only when a package has
distinct ownership or access control, an independent release cadence, and a
stable versioned platform interface.

See [ADR-0073](../adr/0073-flat-coding-focused-workspace.md) for the decision
and the superseded product-family layout.
