# ADR-0161: Route-scoped inference protocols and prompt-cache contracts

- **Status:** Accepted
- **Date:** 2026-08-31

## Context

Muta previously represented the same inference surface three times: a model
wire format, a remotely advertised endpoint, and a persisted user transport.
String parsing silently mapped unknown values to OpenAI Chat Completions, while
boolean flags independently enabled ChatGPT and Copilot behavior. Invalid
combinations were therefore representable, and a model could change API shape
when reached through another provider without one authoritative route value.

Prompt caching had the same category error. A model-family classifier treated
cache behavior as intrinsic to a family, even though support depends on the
provider, endpoint, protocol generation, model generation, and account. It
also collapsed server-managed caching, request controls, retention, affinity,
and response counters into one strategy. Some declared capabilities had no
wire encoder, and OpenAI's default could select explicit caching without
emitting an explicit breakpoint.

The result was unsafe for a multi-provider client: protocol resemblance could
grant unsupported controls to a relay, provider-specific counters could be
lost, and configuration errors could silently become OpenAI requests.

## Decision

Represent the exact inference API with one closed `WireProtocol` domain:

- `openai-chat-completions`
- `openai-responses`
- `anthropic-messages`
- `google-generate-content`

Use that type for model baselines, live model metadata, provider presets,
custom connections, discovery routing, and add/edit events. Parsing and
deserialization accept only canonical names; there is no unknown-to-OpenAI
fallback and no legacy alias.

Represent provider variations as protocol-specific, mutually exclusive
dialects. ChatGPT and Copilot are dialects of their selected OpenAI or
Anthropic protocol; Antigravity is a dialect of Google generateContent.
Dialects may change authentication headers, endpoint envelopes, and streaming
normalization, but never the protocol identity.

Resolve prompt caching from a capability contract attached to one concrete
provider route and model. The contract declares:

- supported modes (`implicit`, `automatic`, `explicit`) and one explicit
  provider default;
- supported and default retention values;
- whether disable and routing-key controls are executable;
- explicit-breakpoint and minimum-token limits;
- read, write, and miss telemetry reported by that route.

A request carries only a preference. Resolution merges the request preference
over the route preference, validates it against the route capabilities, and
produces a closed resolved plan. Unsupported requests fail; they never degrade
to a different mode. An unspecified preference uses the declared provider
default, not enum order. Request ephemerality is independent of cache intent.

Protocol encoders own the standard wire representation. Provider registries
own whether a route supports that representation. OpenAI encoders share the
retention, affinity, options, and explicit-breakpoint implementation;
Anthropic owns top-level automatic and block-level explicit `cache_control`;
Google's implemented route declares implicit caching only. No explicit Google
cached-content resource lifecycle is claimed until one exists end to end.

Normalize response telemetry through one parser and preserve cache reads,
writes, and provider-specific misses through per-turn accounting.

## Alternatives considered

### Keep separate transport, wire-format, and endpoint enums

Rejected because they describe one routing fact and require fallible mappings.
Keeping all three permits drift and makes provider-dependent model routing
harder to reason about.

### Infer cache policy from model family or compatible protocol

Rejected because compatibility does not prove that a relay forwards optional
cache fields, uses the same defaults, or reports the same counters. Cache
support is a route capability, not a model trait or protocol entitlement.

### Preserve aliases and default unknown protocols to OpenAI

Rejected. A clean break makes invalid state visible at the boundary and avoids
sending credentials or payloads to the wrong API surface.

### Expose only a generic cache enabled boolean

Rejected because implicit server behavior, automatic boundaries, explicit
breakpoints, retention, affinity, and disabling have different semantics and
cannot be safely projected from one boolean.

## Consequences

- Adding a protocol requires a new typed variant, encoder, discovery mapping,
  documentation, and tests.
- Adding a provider requires an explicit protocol, dialect, and per-model cache
  declaration. Unknown and custom routes start with cache support disabled.
- Persisted custom connections and add/edit API payloads must use the four
  canonical protocol names. Older aliases are intentionally rejected.
- Route cache configuration becomes deterministic and validation errors are
  surfaced before a request is sent.
- Telemetry can distinguish cache population, hits, and provider-reported
  misses without treating misses as additional billable tokens.
- ADR-0067 is superseded.

## References

- [OpenAI prompt caching](https://platform.openai.com/docs/guides/prompt-caching)
- [Anthropic prompt caching](https://platform.claude.com/docs/en/build-with-claude/prompt-caching)
- [Google Gemini context caching](https://ai.google.dev/gemini-api/docs/caching)
- [DeepSeek context caching](https://api-docs.deepseek.com/guides/kv_cache)
- [Prompt caching and cost control](../explanation/agent-design/prompt-caching.md)
- [Providers](../reference/providers.md)
