# Model Context

The model context is the request-scoped view muta sends to a provider. It is
derived from the durable session, but it is not the durable session itself. The
provider receives only the current projection: a rebuilt system prompt, the
model window, and the tool catalog valid for this turn.

For the persistent side of the same boundary, see
[Session persistence](session-persistence.md). For the prompt assembly rules
that feed this context, see [Prompt and message assembly](prompt-assembly.md).

## Stateless Provider Requests

LLM providers are treated as stateless. Every turn sends enough context for the
provider to answer without relying on memory from an earlier HTTP request. The
next turn resends the current model window with any newly appended assistant
messages and tool results.

That is why the "context sent to the LLM" is a projection rather than a storage
layer. It is assembled for a single provider call, serialized into that
provider's wire shape, and discarded after the response stream finishes. The
durable session keeps the recoverable state; the request body is only the
provider-facing materialization of that state.

Messages and admitted tools are captured as one immutable request snapshot.
Provider adapters do not retain tool declarations as mutable state between
calls. A retry resends the same snapshot, while a later round assembles a new
one from the then-current conversation and tool visibility.

## What the Request Contains

Each provider request carries three conceptual inputs:

| Input | Source | Purpose |
|-------|--------|---------|
| **System prompt** | Rebuilt from system-prompt sections and live state | Identity, behavior, model/provider guidance, and conditional workflow guidance |
| **Messages** | Current model window | User messages, assistant replies, assistant tool calls, tool results, and hidden harness messages that still belong in model-visible history |
| **Tools** | Current tool catalog | Native tool declarations: name, description, and parameter schema for each enabled tool |

The system prompt is rebuilt on a cloned request view and is never appended to
the durable model window. Tool schemas are also sent on every request when the
provider supports native tool calling. A disabled or masked tool is not
declared to the provider, and dispatch rejects calls to tools that are not
admitted for the current agent.

## Request Composition and Lifecycle

For every model invocation `n` — user turns and tool-loop invocations alike —
the request is a logical composition of four parts:

```text
R_n = S | H_n | I_n | E_n
```

The separator is logical ordering, not literal concatenation or four API
fields; provider adapters still map instructions, tools, messages, and supported
content blocks to their native protocol. The older `Zone 1/2/3` labels are
retired; the components are the model, and prompt caching is modeled separately
as a derived [cache plan](../../adr/0217-request-components-and-derived-cache-plan.md).

| Component | Meaning | Lifecycle |
|-----------|---------|-----------|
| `S` | Stable instructions and deterministically ordered tool declarations | Stable within a compatible configuration epoch; a legitimate rule, tool, model, or route change can start a new epoch |
| `H_n` | Model-visible history preceding this invocation's newly admitted input | Append-oriented within a history epoch; compaction and reconstruction are explicit boundaries |
| `I_n` | Newly admitted input, such as user messages or tool results | Included exactly once in this request and in subsequent history |
| `E_n` | Optional, bounded request-local information | Not automatically promoted into history or the durable interaction transcript |

Assembly consumes prepared data only: no repository traversal, file parsing, tool
execution, or network enrichment happens here. Producers run before assembly
under the applicable execution and permission boundaries, and every enabled
temporary-context producer must have a finite budget and an explicit relevance
condition. `E_n` may be empty — and is empty by default, because code structure
is delivered on demand through the `code_query` tool rather than as an ambient
per-request map ([ADR-0214](../../adr/0214-on-demand-code-structure-context-and-mutation-freshness.md)).

Three data surfaces stay distinct:

1. **Interaction record** — durable admitted user, assistant, and tool facts.
2. **Model history view** — the selected or compacted representation the model
   reads; not necessarily a byte-for-byte copy of the interaction record.
3. **Request projection** — the immutable prepared snapshot combining prefix,
   history, new input, and any temporary information.

Human visibility, durability, and inclusion in later requests are independent
properties. A hidden tool result can remain in history; hiding presentation does
not make it ephemeral. Temporary payloads are never silently promoted into the
interaction record or later history: retained context enters through an explicit
history-bearing event with provenance. A transport retry of the same prepared
invocation reuses its snapshot rather than refreshing temporary information; a
changed input, route-dependent projection, or refreshed environment requires an
explicitly rebuilt request.

For the full decision, see
[ADR-0213](../../adr/0213-model-request-composition-and-context-lifecycle.md).

The assembled request keeps `H_n | I_n` as its conversation messages and carries
`E_n` in a separate request-local field (`ModelRequest.temporary_context`), so temporary
context cannot leak into durable history by construction. A provider-neutral
[`CachePlan`](../../adr/0217-request-components-and-derived-cache-plan.md)
derives the deterministic identity of the cacheable prefix `S | H | I` and the
durable-versus-temporary message split; provider adapters layer their own
breakpoints, retention, and affinity on top. Diagnostics report temporary-context
token volume separately from durable input.

`E_n` is not discarded, though: each assembled request is also archived as a
durable [`RequestProjection`](../../adr/0218-durable-request-projection-archive.md)
outside the transcript, so the exact request scene (temporary payload and prefix
identity) can be reconstructed later. The archive is forensic only — it is never
read back into a model window, transcript, compaction, or replay.

## Tool Schemas and Tool Trace

Tool definitions and tool usage are separate parts of the context.

The **tool schema** tells the model what it may call. It includes the tool name,
human-readable description, and structured parameter schema. This is the
declaration surface; it is resent every turn because the provider is stateless.

The **tool trace** lives in messages. When the model calls a tool, the assistant
message records the tool name and JSON arguments. A read tool call can therefore
include parameters such as `offset` and `limit` as part of the assistant
tool-call arguments. After local execution, muta appends a tool-result message
with the matching tool-call id and the result content. On the next request, both
the call and the result are present in the model window unless a later context
projection has shortened them.

This is the answer to the common question: yes, a later provider request can
contain the tool description, the fact that a read happened with its arguments,
and the tool result. They enter through different channels: declarations in the
tool catalog, usage and results in the message window.

## Native and Fallback Providers

Provider transports serialize the same conceptual context differently.

OpenAI-compatible providers receive a message array plus a separate native tool
schema field. Other providers may map messages and tools into a different
native shape, or may not support native function calling. When native tool
calling is unavailable, muta uses the fallback path: the model emits a
tool-call-shaped text response, the harness promotes it into the structured tool
trace, then execution and result recording follow the same path as native tool
calls.

Before serialization, provider adapters may filter messages that violate the
target protocol. For example, a tool-result message whose tool-call id no longer
has a preceding assistant call is not sent. The durable session can preserve
more history than a specific provider wire format can accept; the request view
must stay valid for the selected transport.

## Projection Before Sending

The model context is always bounded by the current model window. If context
pressure rises, pruning and compaction project the durable session into a
smaller provider-visible window:

- pruning may remove large stale tool-result bodies while keeping the call/result
  structure,
- compaction may replace older complete rounds with a checkpoint summary,
- both operations retain originals in the durable session,
- the next provider request sends the projected window, not the archived
  originals.

The projection therefore affects what the model reads, not what the session can
recover. This is the central boundary: the model context is optimized for the
next provider call; the durable session is optimized for full resume and audit.

## Mental Model

One turn can be read as this flow:

```text
durable session
  -> restore current model window
  -> enrich durable conversation context when lifecycle events require it
  -> assemble one request snapshot from system + messages + enabled tools
  -> serialize provider-specific request
  -> stream assistant response
  -> append assistant/tool trace back into durable session
```

The arrows are one-way for the request. The provider does not update memory on
its own. Only the local session commit makes new messages durable and eligible
for the next model context.
