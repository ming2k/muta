# 0056. Model-context assembly boundary

- **Status:** Accepted
- **Date:** 2026-07-13

## Context

ADR-0039 introduced a registry intended to cover both system and user prompt
channels. Its migration discovered that the two channels do not share a useful
composition model:

- system policy is a ranked set of fragments folded into one head message and
  rebuilt before every provider request;
- user-role harness context is event-driven and carries a bespoke payload such
  as hook output, a pursuit objective, an inter-agent note, or a skill body.

The production implementation therefore registered only system sections. The
user-channel API (`PromptChannel::User` and `render_section`) remained exercised
only by registry unit tests, while real injections continued to use
`Message::injected` at their lifecycle call sites. `PromptContext` also retained
the unused `last_visible_user_text` field from the abandoned migration.

At the agent layer, `prompt.rs` mixed three separate responsibilities: system
policy declaration, request-time system-message rebuilding, and implicit skill
injection. The same request funnel also removed empty assistant frames and
projected command echoes out of the provider view. The name described only one
part of the behavior and obscured the provider-facing model-context boundary.

## Decision

Make **model context** the agent-layer assembly boundary and keep **system
prompt** as its specialized composition mechanism.

1. Put request preparation, system policy, implicit skills, and constructors
   for harness-authored model-visible messages under `model_context`.
2. Rename the shared domain vocabulary to `SystemPromptContext`,
   `SystemPromptSection`, `SystemPromptRegistry`, and
   `SystemPromptRegistryError`. Remove the channel discriminator and
   single-section rendering API.
3. Give a system section only a stable id, rank, activation predicate, and
   renderer. The registry folds active sections into one system message and
   stamps the canonical `SystemPrompt` provenance.
4. Route hidden and visible harness-authored user context through common
   constructors that own role, visibility, provenance, and attachment
   invariants. Lifecycle owners still decide when to append a message and
   still construct their domain-specific payload.
5. Keep genuine user input, assistant responses, and tool-result protocol
   messages on their source-owned paths. They are conversation or protocol
   events, not harness context injections.
6. Rename the pre-provider chokepoint to `prepare_request_messages`. It runs
   for every provider request, not once per user-perceived turn.
7. Keep one-shot title and store-owned summarization prompts with their owning
   layers. They do not enter the agent loop, and moving them would introduce a
   reverse dependency without sharing lifecycle or composition behavior.

This decision supersedes ADR-0039. Its system-section decomposition, ranked
policy, shared request funnel, and system-message clobber fixes remain in force;
the rejected cross-channel registry vocabulary does not.

## Alternatives considered

### Rename `prompt.rs` to `context.rs`

Rejected because bare “context” already names model windows, projection,
compaction, hook inputs, and tool build state. `model_context` states which
boundary the module owns.

### Put every `Message` constructor behind one registry

Rejected because genuine user, assistant, and tool messages have different
authors and protocol lifecycles. A registry would erase those distinctions and
would require a catch-all context object containing unrelated event payloads.

### Keep the unused user-channel API for future use

Rejected because it advertises an architecture production code does not use.
A future context-derived injection can add a focused abstraction when a real
call site demonstrates the need.

### Move all one-shot prompts into the agent layer

Rejected because title generation and compaction summarization are direct
provider calls owned by other layers. Their prompts are already local to their
single call sites and do not benefit from per-request system recomposition.

## Consequences

**Positive.** The module topology follows the provider-facing data flow. The
system registry is honest about its singleton role. Every agent-loop harness
injection shares construction invariants without forcing unrelated payloads
into one context type. Request preparation has one exact, greppable name.

**Negative.** The public Rust API is a breaking rename for embeddings that
implemented `PromptSection` or called the prompt-policy builder methods. The
old cross-channel types are removed rather than aliased.

**Neutral.** Message roles, visibility, text, section ids, ordering, and
provider adapters do not change. Envoy tasks, review inputs, and tool-image
companions gain the provenance they previously lacked. Store-owned compaction
and title prompts remain separate by design.

## References

- [ADR-0039](0039-unified-prompt-registry.md)
- [ADR-0040](0040-session-state-and-context-projection.md)
- [ADR-0050](0050-non-driving-command-echoes.md)
- [Model context](../explanation/agent-design/model-context.md)
- [Prompt and message assembly](../explanation/agent-design/prompt-assembly.md)
