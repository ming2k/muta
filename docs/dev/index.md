# Contributor docs

| Page | Purpose |
|------|---------|
| [Acceptance](acceptance.md) | Authentic end-to-end user journeys without shortcuts |
| [Testing](testing.md) | Running, interpreting, and extending automated unit, integration, and snapshot test suites |
| [Workspace layout](workspace-layout.md) | Product families, shared package groups, and placement rules |
| [Release process](release.md) | Versioning, pre-tag verification checklist, and tag/publish workflow |
| [Documentation governance](../governance/documentation/core/index.md) | Standardized rules for organizing, writing, and reviewing docs |

## Architecture

- [Crate layering](../explanation/crate-layering.md) — the workspace crate topology, each layer's responsibility, and the dependency DAG
- [Persistence and the XDG layout](../explanation/persistence.md) — why every persistent path flows through the central `Dirs` layer and the four-category split
- [Harness architecture](../explanation/agent-design/harness.md) — control plane, provider calls, autonomous loop
- [Request flow](../explanation/request-flow.md) — HTTP transactions, SSE streaming, ReAct loop
- [Provider capabilities](../explanation/provider-capabilities.md) — tool calling and reasoning across model weights, runtime, and client
- [Guided decoding](../explanation/guided-decoding.md) — constrained decoding, FSM compilation, chat templates
- [Rounds and turns](../explanation/agent-design/rounds-and-turns.md) — tool call lifecycle: declaration, gating, execution, and re-entry

## Policy

- [ADR-0014: Unified XDG persistence architecture](../adr/0014-xdg-persistence-architecture.md) — new persistent locations must be added as methods on `Dirs`, classified by what the file *is*; no inline `dirs::home_dir().join(...)` for muta-owned storage
- [ADR-0121: Instance isolation for development and testing](../adr/0121-instance-isolation-for-development-and-testing.md) — daemon runtime paths derive from `Dirs::instance_dir()` only; auto-spawned daemons inherit the environment (sandbox inheritance); local runs beside an installed daemon use `--home` / `MUTA_HOME`, never a bare `target/debug/muta` against the host instance
