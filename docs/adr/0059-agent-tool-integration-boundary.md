# 0059. Agent-tool integration boundary

- **Status:** Accepted (skill-tool placement revised by ADR-0060)
- **Date:** 2026-07-13
- **Revises:** ADR-0005's `neenee-agent`-does-not-depend-on-`neenee-tools`
  sub-decision and the inherited topology diagrams in ADR-0035/0037

## Context

The concrete `todo` and `todo_update` implementations lived in
`neenee-agent` because they mutate the task list and turn counter owned by an
`Agent`. That placement confused lifecycle ownership with implementation
ownership: the tools implement the same `Tool` contract as filesystem, shell,
and web tools, but were outside `neenee-tools` only because their state is
injected.

Moving the implementations raises two separate questions: which crate owns
their code, and which layer binds them to one agent's state. Treating every
direct dependency on `neenee-tools` as forbidden pushes that binding upward
into every application. It also makes an `Agent` incomplete until a caller
performs an undocumented second installation step.

`neenee-agent` is not a dependency-minimal generic runtime today: it already
owns product-independent orchestration and consumes store, provider, and
authentication implementations. Tools serve that runtime. A dependency from
agent to tools follows the same downward direction and creates no cycle as
long as `neenee-tools` never depends on `neenee-agent`.

## Decision

Use a normal downward integration edge:

1. `neenee-agent` directly depends on `neenee-tools`.
2. `TodoWriteTool`, `TodoUpdateTool`, and `TodoToolContext` live together in
   `neenee-tools`. Serializable todo values remain in `neenee-core` because
   agent, store, session, and frontend layers exchange them.
3. A private `neenee-agent::tool_integration` module constructs concrete tools
   whose lifetime is tied to one agent. Every `Agent` automatically receives
   todo tools bound to its own task list and turn counter.
4. Agent-owned `(name, variant)` identities replace caller-supplied collisions.
   This prevents an embedding from accidentally detaching an invariant-bound
   tool from the state of the agent that dispatches it. Ordinary registry
   insertion remains first-wins.
5. `AgentBuilder::with_tool` and `with_tools` are the extension interface for
   embedding-owned concrete tools. Existing flat-list and `ToolSet`
   constructors remain supported.

Context-free tools continue to self-register through `inventory`. Tools that
need configuration known before agent construction are collected into a
`ToolSet` by the application. Orchestration-native tools such as `EnvoyTool`
and skill-loading tools remain in `neenee-agent` because their implementation
constructs or controls agents rather than merely consuming injected state.

The turn loop, model advertisement, and dispatch continue to use only
`neenee-core::Tool` and `ToolSet`. The direct crate dependency determines
construction ownership; it does not couple the dispatch algorithm to concrete
tool types.

## Alternatives considered

### Keep agent independent and inject todo tools from every application

Rejected. It optimizes for a hypothetical standalone engine at the cost of an
incomplete `Agent` lifecycle today. Principal, side, review, and envoy
construction paths can drift or omit required stateful tools, and an
application must know agent internals merely to make the agent coherent.

If a genuinely independent consumer later needs an orchestration runtime
without the built-in bundle, extract a smaller runtime crate based on that
consumer's requirements instead of weakening the current `Agent` abstraction
preemptively.

### Keep todo tools in agent

Rejected because lifecycle ownership does not imply implementation ownership.
The tools neither construct an `Agent` nor participate in turn orchestration;
they consume an injected context and implement the same core contract as other
built-ins.

### Make tools depend on agent

Rejected because orchestration-native tools would create the reverse edge
`neenee-tools -> neenee-agent` and therefore a cycle once the agent consumes
the tool bundle. Such tools stay in the agent crate.

### Put `TodoToolContext` in core

Rejected. It is a concrete integration handle used only to construct todo
tools from agent-owned cells, not stable domain or wire vocabulary. Core keeps
`TodoList`, `TodoItem`, and `TodoStatus`, but not the live synchronization
mechanism.

## Consequences

**Positive.** Every construction path yields a coherent agent with its
state-bound tools already installed.

**Positive.** Concrete tools have one implementation home, while the agent
retains lifecycle ownership and a direct custom-tool extension interface.

**Positive.** The dependency graph remains acyclic and the runtime dispatch
path remains contract-based.

**Negative.** Consumers of `neenee-agent` also compile and link
`neenee-tools`. This matches the current product-level abstraction; a future
minimal runtime would require a deliberate crate extraction.

**Neutral.** Applications still collect configuration-dependent and dynamic
tools, and `AgentBuilder` accepts product-specific additions.

**Neutral.** Todo schemas, results, persistence, and UI behavior do not change.

## References

- [ADR-0005](0005-strict-layering-and-renames.md)
- [ADR-0020](0020-unified-task-list.md)
- [ADR-0057](0057-contract-only-core-boundary.md)
- [Crate layering](../explanation/crate-layering.md)
