# 0224. One extension primitive: tools and harness facets are atomic projections

- **Status:** Proposed
- **Date:** 2026-09-10
- **Supersedes:** [ADR-0211](0211-agent-role-harness-facets-and-ephemeral-ast-code-intelligence.md)'s dual `Tool` / `HarnessFacet` split.
- **Builds on:** [ADR-0223](0223-capability-is-the-extension-set.md).

## Context

Capability today is split across two unrelated types:

- `Tool` (`muta-contracts::capability`): a model-callable JSON-RPC surface.
- `HarnessFacet` (`muta-contracts::facet`): an ambient harness mechanism with a
  fixed hook set (`project_temporary_context`, `intercept_file_mutation`) and an
  `accompanying_tools()` escape hatch that lets a facet inject tools.

The split leaks in both directions. `CodeIntelligenceFacet` bundles two
unrelated concerns (mutation syntax gating and structure/companion tools) in
one object, so a persona cannot take one without the other. And
`accompanying_tools` already proves a facet is just a tool with an extra hook.

ADR-0223 declares capability to be "the admitted extension set". That set needs
a single, atomic unit.

## Decision

### 1. One `Extension` primitive

```rust
pub trait Extension: Send + Sync + std::fmt::Debug {
    fn id(&self) -> ExtensionId;
    /// The model-callable surface, when this extension is a tool.
    fn tool(&self) -> Option<Arc<dyn Tool>>;
    /// The ambient phases this extension participates in.
    fn hooks(&self) -> &'static [HookPhase];
    /// Run one declared hook phase. Called only for declared phases.
    fn run(&self, phase: HookPhase, ctx: &HookContext) -> HookOutcome;
}
```

- A **tool** is an extension with `tool()` returning `Some`.
- A **facet** is an extension with a non-empty `hooks()`.
- An extension may be both (e.g. code intelligence that also exposes an
  inspection tool); this is explicit, not an escape hatch.

### 2. Fixed hook-phase vocabulary

`HookPhase` is a closed enum owned by the harness (`ProjectTemporaryContext`,
`InterceptFileMutation`, …). An extension declares which phases it handles and
cannot invent new ones; the harness owns ordering and lifecycle. The existing
per-phase defaults (empty projection, pass-through mutation gate) become the
harness's no-op behavior for extensions that do not declare a phase.

### 3. Registry and resolution

- A registry maps `ExtensionId` → factory. Built-in extensions self-register
  (mirroring `register_tool!`); the catalog is introspectable.
- A persona/config lists extension ids; resolution instantiates a per-session
  instance of each and validates the set against the execution environment
  (ADR-0223).
- Unknown ids are a hard resolve-time error with the known catalog listed.

### 4. Atomic decomposition

`CodeIntelligenceFacet` is split into independently selectable extensions
(e.g. `code_intelligence.guard`, `code_intelligence.outline`) so a persona
takes exactly the behavior it wants.

## Invariants & Behavioral Boundaries

1. **One primitive.** `Tool` and `HarnessFacet` are projections of `Extension`,
   never parallel capability channels. `accompanying_tools` is deleted.
2. **Atomic.** One extension = one concern; selection is per extension id.
3. **Instance-bound.** Extensions are instantiated per session/agent; no global
   singletons (a parser/arena bound to a workspace dies with the session).
4. **Closed phases.** `HookPhase` is a harness-owned enum; extensions declare,
   they do not define, phases.
5. **Fail-closed resolution.** An unknown or environment-incompatible
   extension id fails resolution before the agent is constructed.
6. **Introspectable.** The effective extension set is queryable for the UI and
   for validation.

## Alternatives considered

- **Keep `Tool` and `HarnessFacet` separate.** Rejected. Capability stays split
  across two types and `accompanying_tools` remains an implicit third channel.
- **Make facets a kind of tool with hidden run modes.** Rejected. Hides the
  hook contract; the model-callable surface and the ambient phase contract are
  genuinely different, and both must be explicit.
- **Open/dynamic hook phases.** Rejected. The harness cannot guarantee
  ordering, lifecycle, or bounded cost for phases it does not own.
- **Keep `CodeIntelligenceFacet` monolithic.** Rejected. It forces an
  all-or-nothing choice and violates atomicity.

## Consequences

- `ToolSet`/`collect_toolset` operate on extensions; the model-facing catalog is
  the `tool()` projection.
- Facet invocation sites (`project_temporary_context`,
  `intercept_file_mutation`) become generic hook dispatch over declared phases.
- `AgentRole.facets` and `ToolSelection` collapse into one `extensions` list on
  the persona (ADR-0225).
- The command allowlist variants (ADR-0223) become ordinary tool variants
  contributed by extensions.

## References

- ADR-0211 — dual-wing model (superseded).
- ADR-0223 — capability is the extension set.
- ADR-0225 — persona owns identity and capability.
- `crates/muta-contracts/src/facet.rs`, `crates/muta-contracts/src/capability.rs`
