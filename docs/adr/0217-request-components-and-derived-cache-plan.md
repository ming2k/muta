# 0217. Request Components and a Derived Wire Cache Plan

- Status: Accepted
- Date: 2026-09-10
- Scope: agent/context, model request assembly, provider caching, documentation vocabulary
- Deciders: Maintainer
- Consulted: Maintainer–assistant design discussion
- Informed: Agent and provider integration maintainers
- Supersedes: [ADR-0137](0137-server-side-kv-cache-alignment-and-zoning.md), three-zone model and its cache terminology only; unrelated conclusions retained
- Amends: [ADR-0213](0213-model-request-composition-and-context-lifecycle.md), zone-mapping clause; [ADR-0211](0211-agent-role-harness-facets-and-ephemeral-ast-code-intelligence.md), Zone 3 vocabulary
- Depends on: [ADR-0213](0213-model-request-composition-and-context-lifecycle.md)

## Context and Problem Statement

ADR-0137 introduced a three-zone request model: Zone 1 static prefix, Zone 2
monotonic history, Zone 3 ephemeral tail. ADR-0213 later described request
composition as `S | H_n | I_n | E_n` but retained zones "as a mapping" over
those components (`Zone 1 = S`, `Zone 2 = H_n | I_n`, `Zone 3 = E_n`).

That mapping is a strict coarsening. `Zone 2 = H_n | I_n` merges "append-oriented
history" with "admission-once input", whose lifecycles differ. Zone labels
describe position and ordinal, not the axes ADR-0213 makes binding: admission,
persistence, retry, and history evolution. The labels also conflate the logical
component model with wire position, while KV-cache prefix matching operates on
the provider-encoded byte order — a different layer entirely.

The current codebase has no `Zone` type; zones exist only as prose in ADR-0137,
comments, and docs. Zone 3's only production producer — the ambient repository
map — has been removed by ADR-0214, so Zone 3 is empty by default. This is the
moment to retire the vocabulary before it re-acquires definitions and
implementations.

This decision meets the architectural significance threshold by superseding part
of an accepted ADR's model, changing the vocabulary shared across
`muta-contracts`, `muta-agent`, and `muta-llm-client`, and establishing
enforceable cache-planning and diagnostic boundaries. It does not change any
provider wire format.

## Decision Drivers

- Separate logical lifecycle from wire position; keep one primary model.
- Preserve the real invariants ADR-0137 established, re-expressed rather than
  deleted.
- Make cache alignment a deterministic, provider-owned projection that is
  independently testable.
- Avoid multiplying wire fields: ADR-0213 already states the separator is
  logical ordering, not four API fields.
- Avoid premature abstraction: no new type is justified before the component
  boundaries and their tests exist.

## Considered Options

1. Keep zones as the primary request model.
2. Delete zones and leave cache behavior implicit in provider adapters.
3. Retire the zone labels; adopt `S | H_n | I_n | E_n` as the domain model and
   derive a provider-owned `CachePlan` at the wire-projection layer.

## Decision Outcome

Choose option 3.

### Request Components Replace Zones

The primary model is the component composition defined by ADR-0213, quoted here
for reference and unchanged:

```text
R_n = S | H_n | I_n | E_n
```

| Component | Role | Current code surface |
| :--- | :--- | :--- |
| `S` | Stable instructions and deterministically ordered tool declarations | `ModelRequest.instructions` + `ModelRequest.tool_specs` |
| `H_n` | Model-visible history preceding this invocation's newly admitted input | leading region of `ModelRequest.messages` |
| `I_n` | Newly admitted input (user messages, tool results) | newly appended `messages` entries |
| `E_n` | Optional, bounded request-local information | trailing hidden user / `SystemReminder` messages |

`Zone 1`, `Zone 2`, and `Zone 3` cease to be domain terms. Documentation and code
comments name the component or the derived cache plan instead. ADR-0137's text
is not rewritten; this record carries the correction.

### Cache Is a Derived Wire Projection

Prompt caching is a property of the provider-encoded, left-to-right byte
sequence. It is therefore modeled above the components, not inside them:

- Each provider adapter derives a **`CachePlan`** from a prepared
  `ModelRequest`: an ordered list of segments, each with a stable segment
  identity, a content version, and an optional breakpoint.
- Breakpoint placement, block size, retention, and affinity remain
  provider-specific. The plan records what this route will do; it does not
  assert that a hit occurs.
- `CachePlan` is a pure derivation from the request snapshot. It performs no
  I/O and does not mutate the request.
- Any "zone" placement is, at most, a degenerate projection of the plan and has
  no independent lifecycle.

### Retained ADR-0137 Invariants

The following conclusions of ADR-0137 remain binding. Only their zone framing is
retired; the constraints are re-expressed against components and the cache plan:

1. Tool declarations are ordered deterministically (`S`).
2. Tooling is a superset with runtime gating; no per-turn dynamic tool
   subsetting that would disturb `S`.
3. `S` contains no per-request volatile data such as a current timestamp.
4. Historical reasoning content and protocol signatures are replayed verbatim
   (`H_n`).
5. Trajectory compaction bounds old, bulky tool output without rewriting recent
   fidelity (`H_n`).
6. Breakpoint placement stays a concrete provider concern.

### Invariants & Behavioral Boundaries

The following become binding upon acceptance:

1. No request component may be named, typed, or documented as a "zone" in new
   code or docs. The components are `S`, `H_n`, `I_n`, `E_n`.
2. The model is logical: no implementation may split the request into four
   provider fields merely to mirror the notation. Provider adapters keep mapping
   instructions, tools, and messages to their native protocol.
3. `CachePlan` derivation must be deterministic and independently testable from
   a request snapshot, and must not perform repository traversal, file parsing,
   tool execution, or network access.
4. Cache alignment is an opportunity, not a guarantee. No component or plan may
   advertise an unconditional cache hit, and diagnostics must not fabricate hit
   rates when the provider reports no evidence.
5. Retained ADR-0137 invariants (deterministic `S`, superset tooling, no volatile
   `S`, signature-preserving `H_n`, bounded compaction) continue to bind unless a
   later ADR supersedes them explicitly.
6. This supersession is scoped to ADR-0137's zone model and cache vocabulary.
   Its unrelated decisions (in-process native search, persistent PTY sessions,
   inflight deduplication) are unaffected.

### Positive Consequences

- One primary model with lifecycle semantics, instead of a positional alias.
- Cache behavior has a single layer of ownership: provider adapters over the
  ordered wire projection.
- Prefix stability and tail-boundary behavior become directly testable.
- Diagnostics can attribute volume to input, temporary context, and
  provider-reported cache usage without inventing zone fields.

### Negative Consequences & Trade-offs

- Documentation that used zone language must be migrated; until then, two
  vocabularies coexist in the tree.
- Introducing `CachePlan` costs an abstraction. Per ADR-0214's caution, it is
  justified only once component boundaries and their tests exist; a prefix hash
  plus breakpoint set may be the sufficient first form.
- Removing zone names does not change provider behavior or improve cache hits by
  itself; the benefit is clarity and testability, not performance.

## Rejected Alternatives & Negative Knowledge

### Keep Zones as the Primary Model (Option 1)

- Why considered: ADR-0137 is accepted and the labels are familiar.
- Why rejected: the labels are a strict coarsening of the components and cannot
  express admission, retry, persistence, or history evolution. ADR-0213 already
  had to add a mapping to compensate, which is the tell that the primary model
  is wrong.

### Delete Zones, Leave Cache Implicit (Option 2)

- Why considered: smallest change; zone added no type.
- Why rejected: caching is real and provider-specific, and leaving it implicit
  scatters breakpoint decisions and prefix assumptions across adapters with no
  shared, testable contract. The component model also then has no answer for the
  cache concern.

### Four Provider Fields for `S`, `H`, `I`, `E`

- Why considered: the notation reads like four fields.
- Why rejected: ADR-0213 states the separator is logical ordering, not literal
  concatenation or four API fields. Provider adapters must keep mapping to their
  native protocol; `H_n` and `I_n` are the same message array seen from
  different positions.

### Global Immutable History

- Why considered: maximal prefix stability.
- Why rejected: compaction and reconstruction are explicit, legitimate
  boundaries; no implementation may promise a globally immutable history across
  configuration epochs. This repeats ADR-0213's negative knowledge.

### Restore an Ambient Repository Map to "Use" Zone 3

- Why considered: it would give the tail producer a purpose.
- Why rejected: ADR-0214 removes the ambient map and delivers structure on
  demand. A tail is not a channel for bulk code state, and separating cache
  concerns does not revive it.

## Relationship to Existing Decisions

This ADR supersedes ADR-0137's three-zone model and its cache terminology only;
the retained invariants above and ADR-0137's unrelated decisions stay in force.
Its text is not edited, per `[INV-ARCH-01]`; the index records the relationship.

It amends ADR-0213's clause that retained zones as a mapping: components are now
the sole model. ADR-0213's composition, admission, retry, and surface invariants
are unchanged.

It also retires the Zone 3 vocabulary used by ADR-0211 and ADR-0214; their
code-intelligence placement decisions stand, but `E_n` replaces "Zone 3".

## Verification and Adoption

Acceptance of this proposal must not be represented as implementation
completion. Implementation must provide targeted checks for:

- A `CachePlan` (or its minimal prefix-hash form) derived deterministically from
  an unchanged request yields an identical plan and prefix identity.
- Removing or moving `E_n` leaves the common prefix stable through `S | H | I`
  and changes it only from the temporary tail's former position onward.
- Tool declaration order and content in `S` are byte-stable across repeated
  assembly.
- `E_n` is never persisted and never enters `H`.
- Diagnostics distinguish input volume, temporary-context volume, and
  provider-reported cache usage, without asserting hit rates the provider did
  not report.

Implement in small changes: first migrate the vocabulary (docs and comments)
while behavior is unchanged; then introduce the minimal derived cache plan with
prefix-identity tests; only then expand into segment/breakpoint reporting if
measurements justify it. Update the living
[model context](../explanation/agent-design/model-context.md),
[prompt assembly](../explanation/agent-design/prompt-assembly.md), and
[prompt caching](../explanation/agent-design/prompt-caching.md) explanations
alongside the corresponding changes. Do not describe these targets as already
deployed.

## Links

- [ADR-0137: Server-side KV-cache alignment and zoning](0137-server-side-kv-cache-alignment-and-zoning.md)
- [ADR-0213: Model request composition and context lifecycle](0213-model-request-composition-and-context-lifecycle.md)
- [ADR-0214: On-demand code structure context and mutation freshness](0214-on-demand-code-structure-context-and-mutation-freshness.md)
- [ADR-0211: Agent roles, harness facets, and ephemeral AST code intelligence](0211-agent-role-harness-facets-and-ephemeral-ast-code-intelligence.md)
