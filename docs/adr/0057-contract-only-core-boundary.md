# 0057. Contract-only core boundary

- **Status:** Accepted (todo integration detail revised by ADR-0059)
- **Date:** 2026-07-13

## Context

ADR-0005 established `neenee-core` as a zero-I/O domain crate. Over time,
“zero I/O” became an admission rule rather than only a dependency constraint.
Pure but agent-owned behavior accumulated in core even when no provider, tool,
store, session, or frontend consumed it directly. Examples included system-
prompt composition, the mid-turn context-projection hook, pursuit prompt
rendering, text-emitted tool-call recovery, and shell input classification.

Moving all of core into `neenee-agent` does not solve the boundary problem.
`neenee-agent` depends on store and provider implementations, while those
crates consume the shared message, provider, tool, and persistence contracts.
A complete merge would create `agent -> store -> agent` and
`agent -> providers -> agent` cycles. Removing those cycles would require
extracting another contracts crate equivalent to core.

Purity remains necessary for the bottom crate, but it is not sufficient reason
for a type or algorithm to live there.

## Decision

Retain `neenee-core` as the workspace's dependency-inversion boundary and
narrow its admission rule.

An item belongs in core only when at least one of these conditions holds:

1. Multiple independent workspace layers exchange, serialize, render, or
   implement it.
2. A lower-level contract is required to prevent a dependency cycle.
3. It is stable domain or wire vocabulary whose identity must be shared across
   process, persistence, provider, or frontend boundaries.

Being deterministic, side-effect-free, or generally reusable is not by itself
enough. Agent-owned policy, mutable orchestration state, and compatibility
fallbacks live in `neenee-agent`, even when their implementation is pure.

Apply the rule with these initial relocations:

- Move `SystemPromptContext`, `SystemPromptSection`,
  `SystemPromptRegistry`, and its configuration error to `neenee-agent`.
  Keep `ProviderPromptHints` beside the core `Provider` contract.
- Move `ContextProjectionGate` to `neenee-agent`; it is an extension point of
  the agent's turn loop, not a provider or tool capability.
- Move `TodoToolContext` beside the concrete todo tools in `neenee-tools`,
  re-export it through the agent's tool-integration facade, and retain the
  serializable todo value types in core. The agent continues to own and inject
  the live todo state.
- Move pursuit prompt rendering and the completion marker to the agent that
  interprets them while retaining `Pursuit` and persisted pursuit values in
  core.
- Move text-emitted tool-call recovery to the agent compatibility path. Put
  the balanced-JSON framing primitive in `neenee-ai-sdk-core`, where provider
  adapters and the compatibility path can share it without turning core into
  a generic utilities crate.
- Replace the two exported shell-input predicates with one private agent
  classifier that returns plain, secret, or non-interactive input policy.

Do not split core into several smaller crates as part of this decision. The
remaining contracts are strongly connected through `Message`, `ToolOutput`,
events, provider/tool traits, model metadata, and serialized configuration.
Create another foundational crate only when an independently useful dependency
boundary is demonstrated by real consumers or measurable build coupling.

## Alternatives considered

### Merge all of core into agent

Rejected because store, providers, tools, protocol SDKs, sessions, and
frontends consume the contracts without consuming agent orchestration. The
merge creates dependency cycles and forces those crates to compile unrelated
runtime behavior. Extracting interfaces afterward recreates core under a new
name.

### Keep every pure helper in core

Rejected because it turns core into a miscellaneous utilities package. The
result is a broad public API, unnecessary recompilation fan-out, and ownership
that follows implementation technique rather than lifecycle.

### Split core into protocol, domain, policy, and types crates now

Rejected because the current type graph crosses those labels heavily. The
extra manifests and re-export surfaces would increase navigation cost without
removing a demonstrated dependency or build bottleneck.

### Rename core to contracts

Deferred. `neenee-contracts` describes the narrowed responsibility better,
but a workspace-wide rename adds churn without changing dependency direction.
The admission rule matters more than the crate name.

## Consequences

**Positive.** Core remains the stable cycle-breaking contract layer while
agent behavior is colocated with the runtime that owns it.

**Positive.** Pure implementation details no longer become public core APIs by
default. Tests move with their lifecycle owner.

**Positive.** Shell input classification performs one scan and represents its
result as a single policy value instead of two independently exported boolean
queries.

**Negative.** Embeddings importing the relocated symbols from `neenee-core`
must import them from `neenee-agent`. The removed shell and text-tool-call
helpers are no longer public extension points.

**Neutral.** Serialized messages, events, configuration, session data, provider
behavior, tool behavior, and user-visible output do not change.

## References

- [ADR-0005](0005-strict-layering-and-renames.md)
- [ADR-0035](0035-application-layer-split.md)
- [ADR-0056](0056-model-context-assembly-boundary.md)
- [Crate layering](../explanation/crate-layering.md)
