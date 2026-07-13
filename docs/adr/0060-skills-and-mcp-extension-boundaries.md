# 0060. Separate skill capability and MCP connector boundaries

- **Status:** Accepted
- **Date:** 2026-07-13
- **Revises:** ADR-0005's placement of skill tools in `neenee-agent` and
  ADR-0059's statement that skill-loading tools remain there

## Context

Skill and MCP support both extend what an agent can do, but their code was
organized around incidental call sites rather than lifecycle ownership.

The complete skill subsystem lived under `neenee-agent`: metadata, discovery,
remote downloads, caching, registry, rendering, periodic refresh, and tool
adapters. Session handlers nevertheless imported the registry and invoked the
tools directly. Only implicit skill injection is actually orchestration policy.

MCP support was split in the opposite direction. JSON-RPC transport and tool
adapters lived in `neenee-tools`, live connection state and refresh lived in
`neenee-session`, and `Agent` exposed an MCP-specific
`Arc<RwLock<Vec<Arc<dyn Tool>>>>`. This leaked synchronization, made dynamic
tool behavior protocol-specific, and inferred provenance by parsing tool-name
prefixes.

Skill and MCP should not be combined into one generic extension package. A
skill is knowledge resolved into model context; MCP is a connector protocol
that owns external processes and dynamically discovered tools.

## Decision

Create two focused implementation crates:

1. `neenee-skills` owns skill metadata, discovery, remote caching,
   `SkillRegistry`, `SkillCatalog`, and `UseSkillTool` / `ListSkillsTool`.
   `neenee-agent` depends on it because the agent owns implicit model-context
   injection. A registry is optional and attached through
   `AgentBuilder::with_skills`; ordinary agents default to an empty registry.
2. `neenee-mcp` owns the stdio JSON-RPC client, server handles, MCP-to-`Tool`
   adapters, `McpRuntime`, and `McpCatalog`. A session owns each runtime because
   it controls connection lifetime, user enable/disable/reconnect actions, and
   background refresh. The agent has no MCP protocol dependency.
3. Add `DynamicToolSink` to `neenee-core` as the narrow port shared by
   connector runtimes and agents. Sources publish complete named snapshots and
   remove them on shutdown. `Agent` owns the registry, locking, collision
   policy, disabled mask, advertisement, dispatch, and source-aware snapshots.
4. Dynamic source identifiers carry provenance explicitly, such as
   `mcp:filesystem`; the agent does not derive ownership from a tool name.
   Static agent tools win collisions with dynamic tools. Dynamic collisions
   resolve deterministically by source id.
5. Keep `SkillsConfig`, `McpServerConfig`, and `McpConnectionStatus` in
   `neenee-core`: store, session, frontend, and implementation crates exchange
   these values independently.
6. Publish session MCP sources to the principal agent only. Envoys and side
   agents do not implicitly inherit external connections, even when a server
   declares its tools read-only. A future embedding may delegate connector
   capabilities explicitly, but access tier alone is not an authority-
   propagation policy.

The resulting important edges are:

```text
neenee-agent   -> neenee-tools
neenee-agent   -> neenee-skills
neenee-session -> neenee-agent
neenee-session -> neenee-mcp
neenee-mcp     -> neenee-core
```

## Alternatives considered

### Keep skills inside agent

Rejected. Discovery, metadata, remote I/O, caching, and tool adapters do not
participate in turn orchestration, and Session is already an independent
consumer. Keeping them in Agent obscures the model-context policy that truly
belongs there.

### Make agent depend directly on MCP

Rejected. Agent consumes tools, not JSON-RPC connections. MCP is optional,
session-controlled, and resource-owning; direct integration would make every
temporary agent aware of subprocess lifecycle and reconnect policy.

### Put Skill and MCP into `neenee-extensions`

Rejected. Their only commonality is that they can expose tools. Combining
knowledge resolution and connector transport would create a miscellaneous
package with no cohesive lifecycle.

### Keep the raw shared tool holder

Rejected. Returning an agent-owned `RwLock` lets publishers bypass invariants
and provides neither provenance nor per-source removal. A behavioral port is
smaller and keeps synchronization private.

## Consequences

**Positive.** Each capability has one implementation home and an explicit
lifecycle owner.

**Positive.** Agent tool behavior is uniform across MCP and future dynamic
sources, including toggling, snapshots, dispatch, and deterministic collision
handling.

**Positive.** Agent constructors no longer require callers to manufacture an
empty skill registry.

**Positive.** Temporary agents cannot acquire database, private API, or other
connector capabilities merely because they share a provider or have a
read-only profile.

**Negative.** The workspace gains two crates and applications that assemble a
live session depend on both capability crates.

**Neutral.** The public MCP tool names, skill paths, configuration schema, and
model-facing behavior remain unchanged.

## References

- [ADR-0005](0005-strict-layering-and-renames.md)
- [ADR-0057](0057-contract-only-core-boundary.md)
- [ADR-0059](0059-agent-tool-integration-boundary.md)
- [Crate layering](../explanation/crate-layering.md)
