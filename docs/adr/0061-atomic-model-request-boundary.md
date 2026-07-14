# 0061. Atomic model request boundary

- **Status:** Accepted
- **Date:** 2026-07-14
- **Revises:** ADR-0056's mutable message-preparation funnel and clarifies
  ADR-0057's contract-only core boundary

## Context

ADR-0056 correctly placed model-context policy in `neenee-agent`, but the
resulting module combined two lifecycles:

- durable, event-driven additions to the live conversation window, such as
  steering and implicit skill content;
- ephemeral projection of that window into one provider request.

Request preparation mutated the live `Vec<Message>` in place. The system
prompt was therefore temporarily indistinguishable from durable conversation
state, and call sites had to know which mutations were safe to persist.

Tools crossed the provider boundary through a separate stateful operation.
The agent first called `Provider::prepare_tools`, then later passed only
messages to `chat` or `stream_chat_events`. Title generation, summarization,
retry, and concurrent callers could consequently observe tool declarations
from a different logical request. The provider API did not express the actual
invariant: messages and admitted tools form one request snapshot.

The boundary needs clearer lifecycle ownership without turning model context
into an independent subsystem. System-prompt policy, enabled-tool selection,
skill injection, hooks, and session projection all remain agent behavior.

## Decision

1. Define `ModelRequest` in `neenee-core` as the immutable contract exchanged
   by orchestration and providers. It carries the complete message projection
   and tool declarations for one provider call.
2. Make every `Provider` method consume a `ModelRequest`. Remove
   `prepare_tools` and provider-side mutable tool-schema state. Protocol
   adapters translate the request snapshot directly into their wire format.
3. Keep request assembly in `neenee-agent`. `ModelRequestAssembler` owns the
   pure window-to-request transform and system-prompt registry, but it has no
   dependency on `Agent` or live runtime state. `Agent` chooses when to
   assemble and supplies plain snapshots of prompt context and visible tools.
4. Treat the system prompt as request-scoped. Assembly removes any system
   messages and non-driving command echoes from a cloned model window, inserts
   one freshly composed system message, and leaves the durable window
   unchanged.
5. Put durable and event-driven message construction under
   `conversation_context`. Lifecycle owners append those messages to the live
   window, where provenance can survive persistence. Explicitly mentioned
   skills enrich the live window before request assembly.
6. Retry the exact `ModelRequest` snapshot prepared for the failed attempt.
   Hooks and tool visibility are not recomputed merely because transport is
   retried.
7. Do not create a model-context crate. The shared DTO belongs in core because
   independent agent and provider layers exchange it; the policy belongs in
   agent because that layer owns every input lifecycle. Extract a crate only
   if a second orchestration runtime must reuse the policy with an independent
   release, feature, or dependency boundary.

## Alternatives considered

### Move all model-context code to `neenee-core`

Rejected. Core would acquire system-prompt composition, skill policy, tool
visibility, and lifecycle ordering that no independent layer exchanges. That
violates the contract-only admission rule from ADR-0057.

### Move assembly into provider crates

Rejected. Providers own protocol serialization, not session projection or
agent policy. This would duplicate policy across transports and make providers
depend on orchestration concepts.

### Create `neenee-model-context`

Rejected. The proposed crate would have one producer, `neenee-agent`, and no
independent lifecycle or consumer. It would split files without creating an
architectural boundary and would either leak agent state or duplicate core
contracts.

### Keep `prepare_tools` as provider side state

Rejected. Sequencing conventions cannot guarantee atomicity across retries,
auxiliary model calls, or concurrent use. The type boundary must carry all
request inputs together.

## Consequences

**Positive.** A provider call receives one self-contained snapshot. Tool
declarations cannot leak between title, summary, envoy, retry, or principal
requests.

**Positive.** Request projection is testable as a pure transform and cannot
accidentally persist the rebuilt system prompt or remove durable command
echoes.

**Positive.** The directory structure names lifecycle ownership:
`conversation_context` contains durable additions and `model_request`
contains ephemeral assembly.

**Negative.** `Provider` is a breaking public API change. Provider adapters,
test doubles, and embeddings must accept `ModelRequest` and remove
`prepare_tools` calls.

**Neutral.** The workspace gains no crate. `ModelRequest` in `neenee-core` is
a shared DTO, not a transfer of prompt or orchestration policy into core.

## References

- [ADR-0048](0048-session-as-single-source-of-truth.md)
- [ADR-0050](0050-non-driving-command-echoes.md)
- [ADR-0056](0056-model-context-assembly-boundary.md)
- [ADR-0057](0057-contract-only-core-boundary.md)
- [Model context](../explanation/agent-design/model-context.md)
- [Crate layering](../explanation/crate-layering.md)
