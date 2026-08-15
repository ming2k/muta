# Workspace layout

The repository is one Cargo workspace whose members live directly under
`crates/`. Cargo package boundaries express dependency and test boundaries; the
single flat directory keeps every member one level deep so package ownership
and the `cargo -p <name>` selector stay obvious.

## Directory map

```text
crates/
  neenee-cli/           # the unified `neenee` application binary (default workspace member)
  neenee-tui/           # terminal frontend library: app shell, view tree, showcase
  neenee-tui-engine/    # in-house grid + diff rendering engine
  neenee-runtime/       # session harness, handlers, serve transport, control-plane client
  neenee-agent/         # orchestration: the round/turn loop, built-in tools (bash, read, grep, glob, webfetch, …)
  neenee-mcp/           # MCP connector: stdio JSON-RPC client, server lifecycle, tool adapters
  neenee-skills/        # skill discovery, registry, and tool adapters
  neenee-persistence/   # durable state: session store, config, paths
  neenee-contracts/     # shared domain and wire contracts (no deps)
  neenee-llm-client/    # multi-protocol HTTP client (transport + openai/anthropic/google protocols)
  neenee-providers/     # channel registry, factory, discovery + provider facade + OAuth flows
  neenee-tool-derive/   # proc-macro derive for tool adapters (implementation detail of tools)
```

## Ownership rules

- Every workspace member lives directly under `crates/`. There is no
  intermediate grouping directory; a package is selected by name, not by
  location.
- Put shared contracts in `neenee-contracts`, orchestration in `neenee-agent`, and
  the application binary in `neenee-cli`. See
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
