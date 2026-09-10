# 0222. Retire the configured external tool source and its reserved hooks

- **Status:** Accepted
- **Date:** 2026-09-10
- **Supersedes:** the configured/static `[tool.*]` portions of
  [ADR-0085](0085-config-time-tool-scoping.md), and the "plugins / other
  discovery mechanisms" anticipation for user-defined tools in ADR-0060's
  `DynamicToolSink` framing.
- **Related:** [ADR-0221](0221-custom-external-tools-rejected.md) — the
  evaluation that rejects custom external tools.

## Context

[ADR-0221](0221-custom-external-tools-rejected.md) rejects configured/custom
external tools. The codebase nevertheless still carries reservation hooks that
anticipate them:

- `ToolSource::User` — a third classification bucket, documented as
  "SDK/RPC-injected tool. Future capability ... even though nothing populates it
  yet" (`crates/muta-agent/src/tool_manager.rs:42`).
- A matching `user` field and constructor parameter on `ToolManager`, plus a
  classification branch, tests, and doc comments asserting a `builtin > user >
  mcp` collision order.
- `DynamicToolSink`'s doc comment inviting "plugins, or other discovery
  mechanisms" (`crates/muta-contracts/src/dynamic.rs:34`).
- `[tool.*]` reservations in ADR-0085's design text.

These dead openings are not harmless. ADR-0215 already documented a real defect
caused by exactly this pattern — a scope list and a registration list drifting
apart on their first opportunity (the `update_todo` ghost tool). Reserved
"future" surfaces invite accidental implementation, create classification
branches that must be reasoned about and tested, and imply support that does
not exist.

## Decision

Remove the configured external tool source and every hook reserved for it. The
harness has exactly **two** extension surfaces, and no third:

1. **MCP** — executable external tools, discovered at runtime through the
   `DynamicToolSink` port, owning transport and process lifecycle in
   `muta-mcp`.
2. **Skills** — project/user knowledge injected into model context.

### 1. Collapse `ToolSource` to `Builtin | Mcp`

Delete the `User` variant. `ToolSource` becomes a two-bucket classification
(`crates/muta-agent/src/tool_manager.rs`). Remove:

- the `user` field on `ToolManager` and its `new` parameter;
- the `user` classification block in `installed()`;
- the `ToolSource::User` match arm in `snapshot_tools()`
  (`crates/muta-agent/src/agent/tools_admin.rs`);
- the constructor call's empty user `Arc` (`crates/muta-agent/src/agent/state.rs`);
- all tests and comments asserting a `user` bucket or a `builtin > user > mcp`
  order.

### 2. Void the `[tool.*]` reservation

ADR-0085's configured-tool portions — the "static custom tools later" table
row, the `(and, later, [tool.*])` config cascade, the `[tool.*]` trust-gate
text, and the future-`ToolConfig` paragraphs — are void. There is no
configured/static `[tool.*]` source, no `[tool.<name>].capabilities` manifest,
no `ToolSource::Configured`, and no script or plugin tool tier.

### 3. Keep the dynamic port; it is not a custom-tool hook

`DynamicToolSink`, `DynamicToolSource`, and `DynamicCatalog`
(`crates/muta-contracts/src/dynamic.rs`) **remain**. They are the MCP connector
port and the shared refresh contract (also used by model catalogs); they are
not a user-defined-tool surface. Their doc comments are cleaned of the
"plugins" anticipation.

### 4. Reintroduction requires a superseding ADR

Any future configured, scripted, or plugin tool source — including the
capability-manifest direction contemplated in the rejected ADR-0221 — requires
a new **Accepted** ADR that explicitly supersedes this one and clears the
falsification conditions in ADR-0221. It may not be reintroduced as an
"opening" left in place.

## Invariants & behavioral boundaries

- **Tool ownership boundary.** The built-in tool contract is a project
  responsibility: names, schemas, and hazard classifications are authored in
  the codebase and closed to user definition. Users interact with the tool
  surface only by *selecting, scoping, authorizing, or disabling* existing
  tools (`/tools`, `[tool_variants]`, `[permissions]`, `bash_policy`) — never
  by adding definitions. The only user/third-party extension of the executable
  surface is the **MCP connector protocol**; knowledge is extended through
  **skills**. User-defined tools are not a missing dimension: they are
  **collinear with `run_command` + skills + MCP** (redundant, not orthogonal)
  and span no capability those do not already span.
- **Two buckets only.** `ToolSource` is exactly `{ Builtin, Mcp }`. No third
  static source may be added without a superseding ADR.
- **No config-driven tool definition.** No `[tool.*]` table, inline JSON
  Schema, or capability manifest defines a callable tool.
- **No dead reservation.** Any code comment, field, variant, or branch that
  exists "for a future tool source" is a defect, not foresight (ADR-0215's
  drift class).
- **MCP and skills are the only extension surfaces.** External executable
  capability arrives through MCP; knowledge arrives through skills.
- **DynamicToolSink is connector infrastructure.** It may be depended on by
  connector runtimes and model catalogs; it is never a path for user-defined
  tools.

## Alternatives considered

### Keep `ToolSource::User` as a harmless reserved bucket

Rejected. ADR-0215 documents the concrete cost of a "reserved for later" bucket
that a second list drifts away from. A classification branch with no producer
is untested in production and invites reintroduction by accretion rather than
by decision.

### Keep the `[tool.*]` reservation documented as future work

Rejected. ADR-0085 is Accepted and cannot be edited in place; leaving its
reservation unaddressed means the ADR log simultaneously rejects (0221) and
reserves the feature. Recording the voiding in this accepted record keeps the
log consistent.

### Delete `DynamicToolSink` too, on the theory that it is "external tools"

Rejected. The sink serves MCP tools and, via `DynamicCatalog`, the model
catalog. Removing it would break MCP (ADR-0060) and the models-dev refresh
pipeline. The sink is connector infrastructure, not a custom-tool surface.

### Remove the `AgentBuilder::with_tool` / `with_tools` embedding API

Rejected and out of scope. Those insert into the capability `ToolSet` (resolved
as builtin capabilities) and are used by the runtime to install agent-owned
tools (subagent, mesh, archivist). They are an internal Rust embedding seam,
not an external/config-authored tool source.

## Consequences

**Positive**

- The tool model is honest: two buckets that both have real producers.
- The extension story is one pair (MCP + skills) and needs no third security
  or documentation story.
- Removes a classification branch and its tests, and prevents the
  reserved-opening drift that already produced a ghost tool once.

**Negative**

- If a configured-tool need ever materializes (ADR-0221's falsification
  conditions), it must clear a fresh ADR rather than be switched on.
- A one-line CLI wrapper now has no first-class home; it must be a checked-in
  script plus a skill, or an MCP server.

**Migration**

1. Edit `tool_manager.rs`, `state.rs`, `tools_admin.rs` and their tests
   (mechanical, no data migration — the bucket was never populated).
2. Clean the "plugins" comment in `dynamic.rs`.
3. Update this repository's ADR index and any doc that referenced the
   reservation.

## References

- [ADR-0221](0221-custom-external-tools-rejected.md) — the rejection this
  record implements.
- [ADR-0085](0085-config-time-tool-scoping.md) — configured-tool portions
  superseded.
- [ADR-0060](0060-skills-and-mcp-extension-boundaries.md) — `DynamicToolSink`
  and the MCP/skills boundaries.
- [ADR-0215](0215-tool-surface-consolidation.md) — the ghost-tool drift defect
  caused by parallel lists with a reserved surface.
- `crates/muta-agent/src/tool_manager.rs` — `ToolSource`, `ToolManager`.
- `crates/muta-agent/src/agent/tools_admin.rs` — `snapshot_tools`.
- `crates/muta-contracts/src/dynamic.rs` — `DynamicToolSink` doc comment.
