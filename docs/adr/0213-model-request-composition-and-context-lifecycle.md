# 0213. Model Request Composition and Context Lifecycle

- Status: Accepted
- Date: 2026-09-10
- Scope: agent/context, model request assembly, provider caching
- Deciders: Maintainer
- Consulted: Maintainer–assistant design discussion
- Informed: Agent and provider integration maintainers
- Related RFC: None; proposal originates from the context-zoning design discussion
- Related proposal: [ADR-0214](0214-on-demand-code-structure-context-and-mutation-freshness.md)
- Superseding decision: [ADR-0217](0217-request-components-and-derived-cache-plan.md) retires the three-zone labels this ADR had mapped onto its components; the composition and lifecycle contract below are unchanged

## Context and Problem Statement

The three-zone model (retired by ADR-0217) distinguished a stable prefix, growing history, and ephemeral tail. It did not sufficiently distinguish persistence from model-visible sequence continuity. Removing a temporary tail after generating a response changes the prefix before that response on the next request, even when the temporary content itself has not changed.

The current request builder in `crates/muta-agent/src/agent/state.rs` copies input messages and appends facet projections to the request copy. `CodeIntelligenceFacet` can scan the repository while producing that projection. This is current behavior, not the target contract below.

This decision meets the architectural significance threshold through cross-boundary changes to history, request construction, and provider integration, and through enforceable lifecycle and side-effect boundaries. It does not introduce a new provider wire format.

## Decision Drivers

- Preserve relevant, trustworthy context before optimizing cache reuse.
- Make input admission, persistence, request projection, and retry boundaries explicit.
- Prevent unbounded dynamic-context accumulation and hidden request-construction I/O.
- Describe cache opportunities without promising provider-independent hit rates.

## Considered Options

1. Retain zone labels as the complete lifecycle model.
2. Persist every dynamic injection to maximize sequence continuity.
3. Define explicit request composition and independent context lifecycles, retaining zones as a mapping.

## Decision Outcome

Choose option 3. The following is a proposed contract, not a claim that migration is complete.

### Request Composition

For model invocation `n`, including tool-loop invocations rather than only user turns:

```text
R_n = S | H_n | I_n | E_n
```

The separator denotes logical ordering, not literal string concatenation or four API fields. Provider adapters still map instructions, tools, messages, and supported content blocks to their native protocol. The three-zone labels that once mapped onto these components (`Zone 1 = S`, `Zone 2 = H_n | I_n`, `Zone 3 = E_n`) are retired by [ADR-0217](0217-request-components-and-derived-cache-plan.md); the component model is primary.

| Component | Meaning | Lifecycle |
| :--- | :--- | :--- |
| `S` | Stable instructions and deterministically ordered tool declarations | Stable within a compatible configuration epoch; legitimate rule, tool, model, or route changes can start a new epoch |
| `H_n` | Model-visible history preceding this invocation's newly admitted input | Append-oriented within a history epoch; compaction and reconstruction are explicit boundaries |
| `I_n` | Newly admitted input, such as user messages or tool results | Included exactly once in this request and in subsequent history |
| `E_n` | Optional, bounded request-local information | Not automatically promoted into history or the durable interaction transcript |

`I_n` may contain several messages and may already exist in durable storage. The notation does not prescribe when storage commits happen. The projection must partition admitted input without sending it twice.

### Three Distinct Data Surfaces

1. **Interaction record:** durable admitted user, assistant, and tool interaction facts, subject to existing retention rules.
2. **Model history view:** the selected or compacted representation used by the model. It is not necessarily a byte-for-byte copy of the interaction record.
3. **Request projection:** an immutable prepared snapshot combining the stable prefix, model history, new input, and optional temporary information.

Visibility to the human, durability, and inclusion in later model requests are independent properties. A hidden tool result can remain in history; hidden presentation does not make it ephemeral. An external repository index is none of these three surfaces.

### History Evolution and Cache Boundary

Ignoring explicit compaction or reconstruction, let `A_n` denote the admitted assistant output:

```text
Request 1: S | H | U1 | E1
Output:                    A1
Request 2: S | H | U1 | A1 | U2 | E2
Common prefix: S | H | U1
```

The next history view includes `H | U1 | A1`, not `E1`. Moving or removing `E1` prevents direct prefix reuse from its former start onward. This can occur between tool-loop invocations as well as between user turns. Newly supplied tool results or user messages require processing regardless; the additional lost opportunity concerns the old temporary tail and subsequent generated content where the provider could otherwise reuse it.

Provider cache block sizes, breakpoints, expiration, routing, and treatment of generated tokens remain provider-specific. An unchanged logical prefix is an opportunity for reuse, not proof of a cache hit. Tool/schema or instruction changes, compaction, and provider serialization may further shorten it.

### Invariants & Behavioral Boundaries

The following become binding upon acceptance:

1. Request projection must not mutate admitted conversation data or append an already admitted input twice. Repeated preparation from identical snapshots must preserve semantic ordering.
2. `E_n` may be empty. Every enabled producer must have a finite output budget and an explicit relevance condition. Temporary placement must never be justified by an unconditional cache-retention guarantee.
3. Temporary payloads must not be silently promoted into the interaction record or later model history. Deliberately retained context must enter through an explicit history-bearing event with provenance.
4. Historical evidence must not be silently rewritten merely because its source changed. Compaction, reconstruction, and protocol-required transformations must be explicit and preserve required tool-call/result relationships and reasoning/signature constraints.
5. Request assembly must consume prepared data only: no repository traversal, file parsing, tool execution, or network enrichment. Producers operate before assembly under the applicable execution and permission boundaries.
6. A transport retry of the same prepared invocation must reuse its semantic context snapshot rather than refreshing temporary information. A changed input, route-dependent projection, or refreshed environment requires an explicitly rebuilt request; it cannot retain an assumption of identical serialization or cache reuse.
7. Ordinary continuation without `E_n` must preserve append-oriented history within a compatible epoch. No implementation may describe history as globally immutable across compaction or configuration changes.
8. Diagnostics must distinguish input volume, temporary-context volume, and provider-reported cache usage when available. They must not fabricate hit rates when the provider supplies no evidence.

### Positive Consequences

- The notation explains both user turns and multi-call tool loops.
- Persistence and human visibility no longer determine cache assumptions implicitly.
- Request assembly becomes deterministic and independently testable.
- Empty temporary tails avoid paying for irrelevant enrichment.

### Negative Consequences & Trade-offs

- Useful temporary information can still reduce cross-request prefix reuse; budgets limit rather than eliminate that cost.
- Explicit admission and snapshot boundaries require careful integration with retries and compaction.
- Stable-prefix reuse can be lost after legitimate configuration changes; this proposal does not freeze capabilities or override permission policy.

## Rejected Alternatives & Negative Knowledge

### Zone Labels Alone

The labels express position but not admission, retry, durability, or history evolution. They cannot explain why an unchanged AST moved to the new tail still disrupts reuse.

### Persist Every Dynamic Version

Strict append continuity can improve cache opportunities, but repeated environment snapshots consume the context window and preserve conflicting, obsolete evidence. Cached tokens still occupy context and can influence the model.

### Treat the Tail as a Free Dynamic Information Channel

A tail protects the prefix before it on the current request, not every token generated afterward on future requests. Loading all environment state there adds latency and attention cost regardless of provider caching.

## Relationship to Existing Decisions

This ADR originally amended ADR-0137's interpretation of the three-zone lifecycle and cache retention. [ADR-0217](0217-request-components-and-derived-cache-plan.md) now supersedes ADR-0137's zone model and owns the derived cache plan; this ADR defers cache-layer modeling to it and keeps the component composition and independent lifecycles defined above. Deterministic stable-prefix ordering and provider-specific caching remain in force, and ADR-0137's unrelated search, terminal, and concurrency decisions are unchanged. The accepted historical text is not rewritten.

[ADR-0211](0211-agent-role-harness-facets-and-ephemeral-ast-code-intelligence.md) remains Proposed. Its unconditional history cache-hit claims are not adopted here. ADR-0214 addresses its code-intelligence placement separately; this ADR does not decide role naming or facet lifecycle.

## Verification and Adoption

Acceptance of this proposal must not be represented as implementation completion. Implementation work must provide targeted checks for:

- User-input and tool-result admission exactly once across successive requests.
- No mutation of source messages and no automatic persistence of `E_n`.
- The illustrated common-prefix boundary with a removed or moved temporary tail.
- An empty tail and an append-only continuation, plus explicit compaction boundaries.
- Retry snapshot reuse even if an environment producer's underlying data changes.
- Request assembly with an instrumented environment that rejects filesystem and network access.
- Producer budgets and relevance filtering without asserting provider cache-hit percentages.

Implementation PRs must update the living [model context](../explanation/agent-design/model-context.md), [prompt assembly](../explanation/agent-design/prompt-assembly.md), and [prompt caching](../explanation/agent-design/prompt-caching.md) explanations to reflect implemented behavior. This proposal-only change does not relabel current behavior as migrated.

## Links

- [ADR-0137: Server-side KV-cache alignment and zoning](0137-server-side-kv-cache-alignment-and-zoning.md)
- [ADR-0211: Agent roles, harness facets, and ephemeral AST code intelligence](0211-agent-role-harness-facets-and-ephemeral-ast-code-intelligence.md)
- [ADR-0214: On-demand code structure context and mutation freshness](0214-on-demand-code-structure-context-and-mutation-freshness.md)
- [ADR-0217: Request components and a derived wire cache plan](0217-request-components-and-derived-cache-plan.md)
