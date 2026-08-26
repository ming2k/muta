# Workspace layout

The repository is one Cargo workspace whose members live directly under
`crates/`, plus a pnpm workspace for the web frontend under `apps/`. Cargo
package boundaries express dependency and test boundaries; the single flat
directory keeps every member one level deep so package ownership and the
`cargo -p <name>` selector stay obvious.

## Directory map

```text
crates/
  mutx/           # the unified `muta` application binary (default workspace member)
  mutx/           # terminal frontend library: app shell, view tree, showcase
  mutx-engine/    # in-house grid + diff rendering engine
  muta-runtime/       # session harness, handlers, serve transport, control-plane client
  muta-agent/         # orchestration: the round/turn loop, built-in tools (bash, read, find_files, search_text, webfetch, …)
  muta-mcp/           # MCP connector: stdio JSON-RPC client, server lifecycle, tool adapters
  muta-skills/        # skill discovery, registry, and tool adapters
  muta-persistence/   # durable state: session store, config, paths
  muta-contracts/     # shared domain and wire contracts (no deps)
  muta-llm-client/    # multi-protocol HTTP client (transport + openai/anthropic/google protocols)
  muta-providers/     # channel registry, factory, discovery + provider facade + OAuth flows
  muta-tool-derive/   # proc-macro derive for tool adapters (implementation detail of tools)

apps/
  web/                  # browser frontend (Svelte 5 + TS + Vite): daemon control-panel client
```

## Ownership rules

- Every Rust workspace member lives directly under `crates/`. There is no
  intermediate grouping directory; a package is selected by name, not by
  location.
- Frontend packages live under `apps/` and are managed by pnpm
  (`pnpm-workspace.yaml` declares `apps/*`); the committed lockfile is the
  root `pnpm-lock.yaml`. Node dependency state stays out of Cargo's graph:
  nothing under `crates/` may depend on `apps/`.
- Put shared contracts in `muta-contracts`, orchestration in `muta-agent`, and
  the application binary in `mutx`. The web panel's wire types are
  transcribed from `muta-contracts` into `apps/web/src/lib/types.ts` and
  must be updated in the same change as any wire-visible Rust contract edit.
  See [Crate layering](../explanation/crate-layering.md) for the dependency DAG.
- Do not infer a dependency from directory containment. Cargo manifests remain
  the authoritative dependency graph.
- The product focus is the coding agent. Application-specific support packages
  (`mutx-engine`) stay alongside the platform and provider
  crates because they are consumed by the single application.

## Cargo commands

Package names match their directory names. Select focused checks by package
name:

```bash
cargo check -p muta-persistence
cargo test -p muta-agent
```

Running Cargo without `--workspace` or `-p` selects `mutx`, the
configured default workspace member. Workspace-wide checks continue to use the
shared `Cargo.lock`, profiles, dependency versions, and lint policy.

## Repository boundaries

Keep a package in this repository while changes commonly cross it and shared
platform packages. Consider a separate repository only when a package has
distinct ownership or access control, an independent release cadence, and a
stable versioned platform interface.

See [ADR-0073](../adr/0073-flat-coding-focused-workspace.md) for the decision
and the superseded product-family layout.
