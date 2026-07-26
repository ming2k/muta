# Prompt and Message Assembly

A model never sees a raw transcript. Before every provider request the harness
composes what the model actually reads from three independent channels, each
with its own rules for what it carries, when it is rebuilt, and how it reaches
the provider. This page is the integrating view of those channels. The
individual mechanisms each has its own deep-dive; this page ties them together
and covers the discipline that makes the whole assembly auditable.

For the request-scoped context that consumes the assembled prompt, see
[Model context](model-context.md). For the round that sends that context, see
[Harness architecture](harness.md) and [Rounds and turns](rounds-and-turns.md).

## The three channels

What a model receives on a request is not one prompt but three things traveling
in parallel:

| Channel | What it carries | Rebuilt when | How it reaches the model |
|---------|-----------------|--------------|--------------------------|
| **System** | Identity, behavioral policy, and live state such as the pursuit | Every request, from scratch | A single head system message |
| **User** | Genuine user input, plus harness-injected steering notes | Appended as the round proceeds | User-role messages |
| **Tools** | Each tool's name, description, and parameter schema | Every request | The native function-calling `tools` field, outside the conversation |

Keeping the channels separate is the central design idea. The system message is
*recomposed* before each request from live state, so it can never drift stale.
The user channel carries both real input and harness steering, but never the two
confused — every harness insertion is stamped so a persisted transcript can say
exactly what was injected and why. Tools are advertised through the provider's
own schema surface, not described in prose, so the two never contradict.

## The system message

The system message is rebuilt from scratch before every provider request, not
stored as durable policy. It is assembled in a fixed reading order, each
section present only when its precondition holds:

1. **Identity preamble.** Who this agent is — a name and a mission composed into
   one opening sentence. The engine itself is identity-agnostic: it does not
   hardcode a persona or a purpose. The embedding (the CLI, a future frontend)
   supplies them, so the same engine can serve as a coding assistant, a research
   agent, or an operations agent by passing different values. An envoy takes
   a third form: its identity *is* its role's full system prompt, injected
   verbatim as the preamble, ignoring name and mission. See
   [Envoys](envoys.md).
2. **Model and provider guidance.** Narrow behavioral or protocol facts may be
   supplied by the selected model and provider. Empty guidance contributes no
   section.
3. **Persistence and autonomy.** Mission-independent policy tells the agent to
   carry work through implementation and verification. An unattended session
   adds the stronger rule that no human is reachable and ambiguity must be
   resolved without waiting for input.
4. **Active pursuit.** When a session has an active pursuit, its objective is
   inlined into the system message as live context. See [Pursuits](pursuits.md).
5. **Conditional workflow guidance.** Tool schemas are declared natively (see
   [Tools](#tools-declared-not-described)), but cross-tool workflow policy may
   exceed what one schema can express. Delegation and dedicated file-editing
   guidance appear only when a matching capability is admitted.

Skills content is not placed in the system message. The model discovers skill
metadata through `list_skills`; bodies arrive through `use_skill` or an explicit
implicit-invocation marker. Likewise, tools are never listed in the system
message — their names and schemas travel the dedicated `tools` field.

Each section is a declarative `SystemPromptSection`, not a hardcoded push in an
imperative method. A section carries a stable id, a rank that fixes its reading
order, an activation precondition, and a renderer. The system-prompt registry
composes active sections in rank order and stamps the singleton message's
canonical origin. That makes each section independently testable,
reorderable, or disable-able. The same engine serves an envoy: its role persona
becomes the identity preamble and composes with the applicable shared policy.
The registry and context snapshot are agent-owned policy; only the provider's
narrow prompt-hints value crosses the shared contract boundary. See
[ADR-0056](../../adr/0056-model-context-assembly-boundary.md) and
[ADR-0057](../../adr/0057-contract-only-core-boundary.md).

## Conditional injections

The system message is not the only place the harness shapes behavior. As a round
unfolds across rounds, the harness injects user-role messages under specific
conditions to steer the model. Each injection is a deliberate intervention with
a defined trigger, and each is recorded so the transcript remains faithful.

| Injection | Trigger | Intent |
|-----------|---------|--------|
| **Pursuit continuation** | The `/pursue` stop-gate forces another turn because the pursuit is not yet complete | Re-anchor the model on the objective and define what counts as completion; the prompt marks the objective as untrusted user data and sets rigorous completion-audit criteria so the model does not declare victory prematurely |
| **Pursuit objective updated** | The user edits the active pursuit mid-flight | Tell the model the objective changed and to drop work that only served the old one |
| **Doom-loop block note** | The optional deterministic guard blocks a repeated watched tool signature before execution | Tell the model the call was refused and require a different command, file, query, or an explicit `abort` |
| **Compaction checkpoint** | Context pressure triggers compaction | Wrap a model-written summary of archived rounds under a stable header that flags it as durable context, not a new request. See [Context compaction](context-compaction.md) |
| **Implicit skill** | The latest user message mentions a skill name | Load the skill body so the model behaves as if it had explicitly invoked it. See [Skills](skills.md) |
| **Hook output** | A configured lifecycle hook returns injected context | Let user practice (lint failures, CI gates, reminders) re-enter the conversation. See [Lifecycle hooks](hooks.md) |
| **Envoy steering** | A parent agent steers a running child | Land a visible user message directing the envoy, or a hidden inter-agent note. See [Envoys](envoys.md) |
| **Envoy task** | The harness starts an envoy or a session-review diagnostic | Open the child transcript with its delegated task or review input while retaining its non-user provenance |
| **Tool image** | A tool returns an image that must travel as a user-role companion message | Preserve the image attachment and identify the protocol projection as harness-authored context |

A defining property is that none of these are semantic guesses. The optional
doom-loop guard uses deterministic normalized signatures rather than a model
judgement. Its normalization is deliberately conservative, so it remains an
advanced, default-off policy rather than an always-on heuristic. See the
[Configuration Reference](../../reference/configuration.md#agent-behavior).

The injected prompts follow a consistent design. Pursuit-related prompts wrap
user-supplied text in an XML sentinel (`<objective>` / `<untrusted_objective>`)
and explicitly label it as user data, not higher-priority instructions — a
prompt-injection guard that treats the objective as the task to pursue, never as
authority to override the system message. They also encode fidelity and
completion-audit rules in prose: optimize for movement toward the requested end
state, do not substitute a narrower easier task, and treat completion as
unproven until current evidence proves every requirement.

## The user channel: genuine versus injected

The user channel carries two kinds of message that share a role but are
structurally distinct:

- **Genuine user input** — what the user typed, plus images. This is the real
  conversation.
- **Harness context messages** — the steering notes and protocol projections
  from the table above. Internal nudges are hidden so they do not clutter the
  transcript; child tasks, explicit steering, review inputs, and tool-image
  companions remain visible where their lifecycle requires it. All carry
  structured provenance.

Because both share `Role::User`, the only reliable way to tell them apart is the
provenance stamp every injection carries. A genuine message has none; an
injected message records both *what* it is and *why* it is here. This is what
makes a persisted transcript reconstructible: resume, replay, and audit can all
answer "what was injected, when, and why" without fragile string-sniffing.

## Tools: declared, not described

Tools are advertised to the model through the provider's native function-calling
surface — the `tools` field alongside the message history — not described in the
system prompt. Each tool declares three things: a name, a description, and a
JSON schema for its parameters. This declaration is request-scoped: every turn,
including the turn that carries tool results back upstream, re-sends the full
schema set. The provider is stateless across turns.

Two consequences follow from keeping tools out of the prompt:

- **No contradiction.** The model's authoritative source for what a tool is and
  what it accepts is the schema, not a prose paraphrase. The system message
  carries only behavioral guidance the schema cannot express (when to use
  `ask_user`, how to format its options), and only when the tool is present.
- **Dynamic masking.** A tool can be hidden from the model without rebuilding
  the agent: its schema is dropped before declaration and its name is rejected
  at dispatch, but the tool stays installed so it can be re-enabled. The model
  cannot name a tool it was never told about.

MCP servers extend the same surface: their tools are discovered dynamically and
folded into the same declaration path as built-ins. See [MCP
servers](mcp.md).

## Provenance and traceability

The unifying discipline across all three channels is **provenance**. Every
message the harness constructs — a request-scoped system message, a steering
note, a compaction checkpoint, an implicit skill — is stamped at the
construction site with a structured origin that classifies it. Genuine user
input, assistant replies, and tool results carry no origin; only harness
injections do.

The classifier is deliberately closed: adding an injection path requires adding
a variant, and exhaustiveness checking forces every injection site to be
stamped. For event-driven context appended to the live window, the stamp
survives serialization, so a session saved to disk and reopened later
reconstructs the exact live round. The system prompt is different: its stamp
lives only in the ephemeral request because the prompt is rebuilt from current
state and is never persisted as conversation history.

System sections use stable string ids for policy configuration. Their composed
request message carries the single `SystemPrompt` provenance kind;
event-driven user context carries the kind specific to its lifecycle source.

## Decision history

- [ADR-0056](../../adr/0056-model-context-assembly-boundary.md) — model-context
  assembly becomes the provider-facing boundary; system sections use the
  specialized `SystemPrompt*` vocabulary while harness-authored user context
  shares message-construction invariants.
- [ADR-0061](../../adr/0061-atomic-model-request-boundary.md) — messages and
  tools form one immutable request while durable conversation additions remain
  separate from ephemeral assembly.
- [ADR-0039](../../adr/0039-unified-prompt-registry.md) — introduced ranked
  system-prompt composition and fixed latent system-message clobber defects;
  superseded by ADR-0056 for the cross-channel abstraction boundary.
- [ADR-0034](../../adr/0034-range-aware-pruning-and-deterministic-read-loop-guard.md)
  — the deterministic read-loop guard and why a frequency window replaces a
  consecutive counter.
- [ADR-0030](../../adr/0030-early-loop-intervention-and-round-hook.md) — early
  loop intervention, including the turn-hook axis that later fed hook-driven
  injection.
- [ADR-0019](../../adr/0019-model-relative-context-compaction.md) — the
  compaction checkpoint as durable context.

## Adjacent layers

Each injection mechanism has a deep-dive of its own: [Pursuits](pursuits.md)
for the continuation and objective-update prompts, [Context
compaction](context-compaction.md) for the checkpoint, [Skills](skills.md) for
implicit loading, [Lifecycle hooks](hooks.md) for hook-driven context, and
[Envoys](envoys.md) for inter-agent steering. The protocol contract that
carries these messages to the provider is covered in [Chat API
primitives](../chat-api-primitives.md) and [Request flow](../request-flow.md).
